//! JURI XML parser: root detection, decision parsing, raw nodes.

use super::*;

pub(crate) const JURI_EMPTY_XML_ROOT: &str = "EMPTY_XML";

pub(crate) const ROOT_JUDI: &str = "TEXTE_JURI_JUDI";

pub(crate) const ROOT_ADMIN: &str = "TEXTE_JURI_ADMIN";

/// Parse a bulk jurisprudence archive member into a canonical decision (or an unsupported-root
/// classification). `source` is the dataset the archive belongs to (`cass`/`capp`/`inca`/`jade`).
pub fn parse_juri_member(
    source: ArchiveSource,
    member: &ArchiveMember,
) -> Result<ParsedJuriXml, JuriParseError> {
    let xml = std::str::from_utf8(&member.bytes).map_err(|error| JuriParseError::NotUtf8 {
        member: member.member_path.clone(),
        message: error.to_string(),
    })?;
    parse_juri_xml(source, xml, SourceProvenance::from_archive_member(member))
}

/// Parse bulk jurisprudence XML into a canonical decision (or an unsupported-root classification).
pub fn parse_juri_xml(
    source: ArchiveSource,
    xml: &str,
    provenance: SourceProvenance,
) -> Result<ParsedJuriXml, JuriParseError> {
    if !source.is_jurisprudence() {
        return Err(JuriParseError::UnknownSource {
            dataset: source.as_str().to_owned(),
        });
    }
    let root = detect_root(xml)?;
    let family = match root.as_str() {
        ROOT_JUDI => JuriFamily::Judicial,
        ROOT_ADMIN => JuriFamily::Administrative,
        _ => return Ok(ParsedJuriXml::UnsupportedRoot { root }),
    };
    // Reject archive-source/root-family mismatches (e.g. a judicial JURITEXT XML handed to the JADE
    // source) so a record is never misclassified as the wrong official dataset (WARN 4).
    if JuriFamily::for_source(source) != Some(family) {
        return Err(JuriParseError::SourceFamilyMismatch {
            dataset: source.as_str().to_owned(),
            root: family.root_element(),
        });
    }
    let decision = parse_decision(source, family, xml, provenance)?;
    Ok(ParsedJuriXml::Decision(Box::new(decision)))
}

pub(crate) fn detect_root(xml: &str) -> Result<String, JuriParseError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) | Ok(Event::Empty(start)) => {
                return Ok(local_name(start.name().as_ref()));
            }
            Ok(Event::Eof) => return Ok(JURI_EMPTY_XML_ROOT.to_owned()),
            Ok(_) => {}
            Err(error) => {
                return Err(JuriParseError::Xml {
                    message: error.to_string(),
                });
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct RawDecision {
    pub(crate) fields: BTreeMap<String, String>,
    pub(crate) case_numbers: Vec<String>,
    /// Body text accumulated with inline whitespace collapsed and `\n` at block boundaries.
    pub(crate) body: String,
    pub(crate) summaries: Vec<DecisionSummary>,
    pub(crate) current_summary: Option<DecisionSummary>,
    pub(crate) links: Vec<RawLink>,
}

/// Capture the judicial `PUBLI_BULL@publie` flag (`oui`/`non`) under a distinct metadata key so it
/// never collides with any `PUBLI_BULL` text content.
pub(crate) fn capture_publi_bull(raw: &mut RawDecision, start: &BytesStart<'_>) {
    if let Some(publie) = attribute_value(start, "publie") {
        raw.fields.insert("PUBLI_BULL_publie".to_owned(), publie);
    }
}

pub(crate) fn parse_decision(
    source: ArchiveSource,
    family: JuriFamily,
    xml: &str,
    provenance: SourceProvenance,
) -> Result<CanonicalDecision, JuriParseError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut stack = Vec::<String>::new();
    let mut raw = RawDecision::default();

    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                let name = local_name(start.name().as_ref());
                match name.as_str() {
                    "LIEN" => raw.links.push(RawLink {
                        text: String::new(),
                        attributes: collect_attributes(&start),
                    }),
                    "SCT" => {
                        raw.current_summary = Some(DecisionSummary {
                            id: attribute_value(&start, "ID"),
                            kind: attribute_value(&start, "TYPE")
                                .unwrap_or_else(|| "PRINCIPAL".to_owned()),
                            text: String::new(),
                        });
                    }
                    "ANA" => {
                        raw.current_summary = Some(DecisionSummary {
                            id: attribute_value(&start, "ID"),
                            kind: "analyse".to_owned(),
                            text: String::new(),
                        });
                    }
                    // `<PUBLI_BULL publie="oui">…</PUBLI_BULL>` — capture the publication flag.
                    "PUBLI_BULL" => capture_publi_bull(&mut raw, &start),
                    _ => {}
                }
                stack.push(name);
            }
            Ok(Event::Empty(start)) => {
                let name = local_name(start.name().as_ref());
                match name.as_str() {
                    "LIEN" => raw.links.push(RawLink {
                        text: String::new(),
                        attributes: collect_attributes(&start),
                    }),
                    // The common shape is the self-closing `<PUBLI_BULL publie="oui"/>`.
                    "PUBLI_BULL" => capture_publi_bull(&mut raw, &start),
                    // Self-closing block tag (`<br/>`, rarely `<P/>`) inside the body → paragraph
                    // boundary. Gate on the body context exactly like text capture.
                    block if is_body_block_boundary(block) && in_body_context(&stack) => {
                        append_block_boundary(&mut raw.body);
                    }
                    _ => {}
                }
            }
            Ok(Event::End(_)) => {
                if let Some(name) = stack.last() {
                    // Closing a block element (`</P>`, `</li>`, …) inside the body ends a paragraph.
                    if is_body_block_boundary(name.as_str()) && in_body_context(&stack) {
                        append_block_boundary(&mut raw.body);
                    }
                    if matches!(name.as_str(), "SCT" | "ANA")
                        && let Some(summary) = raw.current_summary.take()
                    {
                        let trimmed = collapse_ws(&summary.text);
                        if !trimmed.is_empty() {
                            raw.summaries.push(DecisionSummary {
                                text: trimmed,
                                ..summary
                            });
                        }
                    }
                }
                stack.pop();
            }
            Ok(Event::Text(text)) => {
                let value = text.decode().map_err(|error| JuriParseError::Xml {
                    message: error.to_string(),
                })?;
                assign_text(&mut raw, &stack, value.as_ref());
            }
            Ok(Event::CData(text)) => {
                let value = String::from_utf8_lossy(text.as_ref());
                assign_text(&mut raw, &stack, value.as_ref());
            }
            Ok(Event::GeneralRef(reference)) => {
                let value = resolve_reference(&reference)?;
                assign_text(&mut raw, &stack, value.as_str());
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                return Err(JuriParseError::Xml {
                    message: error.to_string(),
                });
            }
        }
    }

    raw.into_decision(source, family, xml, provenance)
}

/// Tags whose text content we capture as scalar metadata (last-write-wins per tag).
pub(crate) fn is_scalar_metadata_tag(name: &str) -> bool {
    matches!(
        name,
        "ID" | "ANCIEN_ID"
            | "ORIGINE"
            | "URL"
            | "NATURE"
            | "TITRE"
            | "DATE_DEC"
            | "JURIDICTION"
            | "NUMERO"
            | "SOLUTION"
            | "ECLI"
            | "FORMATION"
            | "FORM_DEC_ATT"
            | "DATE_DEC_ATT"
            | "SIEGE_APPEL"
            | "JURI_PREM"
            | "LIEU_PREM"
            | "PRESIDENT"
            | "AVOCAT_GL"
            | "AVOCATS"
            | "RAPPORTEUR"
            | "COMMISSAIRE_GVT"
            | "TYPE_REC"
            | "PUBLI_RECUEIL"
            | "PUBLI_BULL"
    )
}

pub(crate) fn assign_text(raw: &mut RawDecision, stack: &[String], value: &str) {
    // CONTENU body text lives under BLOC_TEXTUEL/CONTENU and may be wrapped in inline/block tags;
    // capture it with inline whitespace collapsing (block boundaries are added on tag start/end).
    if in_body_context(stack) {
        append_xml_content(&mut raw.body, value);
        return;
    }

    let Some(current) = stack.last() else {
        return;
    };
    match current.as_str() {
        "NUMERO_AFFAIRE" => {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                raw.case_numbers.push(trimmed.to_owned());
            }
        }
        "SCT" | "ANA" => {
            if let Some(summary) = raw.current_summary.as_mut() {
                summary.text.push_str(value);
            }
        }
        "LIEN" => {
            if let Some(link) = raw.links.last_mut() {
                link.text.push_str(value);
            }
        }
        name if is_scalar_metadata_tag(name) => {
            // Only inside META blocks (TITRE etc. are unique); ignore stray text elsewhere.
            let entry = raw.fields.entry(name.to_owned()).or_default();
            entry.push_str(value);
        }
        _ => {}
    }
}

/// Whether the current element stack is inside the decision's main text body
/// (`…/BLOC_TEXTUEL/CONTENU/…`). Mirrors the text-capture and `<br/>` guard exactly so they never
/// diverge (NIT 1). `SOMMAIRE` and `CITATION_JP/CONTENU` are excluded because they lack `BLOC_TEXTUEL`.
pub(crate) fn in_body_context(stack: &[String]) -> bool {
    path_contains(stack, &["BLOC_TEXTUEL", "CONTENU"])
}

impl RawDecision {
    pub(crate) fn into_decision(
        mut self,
        source: ArchiveSource,
        family: JuriFamily,
        xml: &str,
        provenance: SourceProvenance,
    ) -> Result<CanonicalDecision, JuriParseError> {
        // Normalize whitespace on captured scalar fields.
        for value in self.fields.values_mut() {
            *value = collapse_ws(value);
        }
        self.fields.retain(|_, value| !value.is_empty());

        let source_uid = required(family.entity_name(), "ID", self.fields.get("ID").cloned())?;
        validate_uid(&source_uid, family)?;

        let decision_date = {
            let value = required(
                family.entity_name(),
                "DATE_DEC",
                self.fields.get("DATE_DEC").cloned(),
            )?;
            validate_date_field("DATE_DEC", &value)?;
            value
        };

        let body = finish_body(&self.body);
        // Some real DILA decisions carry no textual `BLOC_TEXTUEL/CONTENU` (metadata-only / withheld
        // records). They are not corrupt, but there is nothing to chunk or search, so surface a typed
        // empty-body signal the ingest treats as a member-level SKIP rather than a fatal projection
        // failure that would abort the whole run.
        if body.trim().is_empty() {
            return Err(JuriParseError::EmptyBody { source_uid });
        }
        let source_payload_hash = provenance
            .payload_hash
            .clone()
            .unwrap_or_else(|| source_payload_hash(xml.as_bytes()));

        let document_id = format!("{}:{source_uid}", source.as_str());
        let title = self.fields.get("TITRE").cloned();
        let jurisdiction = self.fields.get("JURIDICTION").cloned();
        let ecli = self.fields.get("ECLI").cloned();
        let number = self.fields.get("NUMERO").cloned();
        let solution = self.fields.get("SOLUTION").cloned();
        let formation = self.fields.get("FORMATION").cloned();
        let nature = self.fields.get("NATURE").cloned();
        let publication = match family {
            JuriFamily::Judicial => self.fields.get("PUBLI_BULL_publie").cloned(),
            JuriFamily::Administrative => self.fields.get("PUBLI_RECUEIL").cloned(),
        };
        // The XML `URL` element is an internal DILA filesystem path (provenance only); expose a
        // stable public Légifrance jurisprudence URL derived from the source-native UID instead.
        let source_url = Some(format!(
            "https://www.legifrance.gouv.fr/juri/id/{source_uid}"
        ));

        let mut decision = CanonicalDecision {
            document_id,
            source: source.as_str().to_owned(),
            source_family: family,
            kind: "decision".to_owned(),
            source_uid,
            citation: title.clone(),
            title,
            body,
            decision_date,
            jurisdiction,
            ecli,
            number,
            solution,
            formation,
            nature,
            publication,
            case_numbers: self.case_numbers.clone(),
            source_url,
            source_payload_hash,
            source_archive: provenance.archive_name.clone(),
            source_member_path: provenance.member_path.clone(),
            chunking_provenance: "heuristic".to_owned(),
            raw_metadata: self.fields.clone(),
            summaries: self.summaries.clone(),
            publisher_edges: Vec::new(),
            inferred_edges: Vec::new(),
            chunks: Vec::new(),
            canonical_version: JURI_DECISION_CANONICAL_VERSION.to_owned(),
        };

        decision.publisher_edges = build_publisher_edges(&decision, &self.links);
        decision.inferred_edges = build_inferred_citation_edges(&decision);
        decision.chunks = build_decision_chunks(&decision);
        Ok(decision)
    }
}
