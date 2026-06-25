# Business-law enhancement dataset download scripts

Destination defaults to `/mnt/models/opendata`. Override with:

```bash
DEST_ROOT=/somewhere/else ./01-bofip.sh
```

Recommended order:

1. `01-bofip.sh` - BOFiP tax doctrine exports, public.
2. `02-sirene.sh` - Sirene monthly business/establishment stock, public.
3. `03-autorite-concurrence.sh` - French competition decisions, public.
4. `04-info-financiere-amf.sh` - AMF/info-financiere issuer publications, public.
5. `05-eurlex.sh` - EUR-Lex dumps or targeted CELEX snapshots, public but needs a URL/CELEX list for non-targeted dumps.
6. `06-rne-inpi.sh` - RNE/INPI registry data, requires Data INPI SFTP credentials.
7. `07-acpr-registers.sh` - ACPR REFASSU public file plus REGAFI notes.
8. `08-ted.sh` - TED public procurement monthly packages, public.
9. `09-dg-comp-eu.sh` - DG Competition case-data downloads, public but needs selected distribution URL list.

Credential/setup notes:

- RNE/INPI requires a Data INPI account and SFTP credentials.
- ACPR REGAFI API access likely requires registration on the Banque de France developer portal; the included script downloads only public REFASSU XLSX files.
- Sirene stock downloads do not need an API key; the live API can use tokens, but these scripts use public data.gouv resources.
- EUR-Lex and DG Competition scripts are intentionally manifest-driven for full dumps, so you choose the official asset URLs before large downloads.

## How to get missing API credentials

### RNE / INPI SFTP credentials

The full RNE bulk feed is free but account-gated by Data INPI.

1. Create or sign in to a Data INPI / INPI Connect account:
   <https://data.inpi.fr/register>
2. Open the enterprise API/SFTP access area:
   <https://data.inpi.fr/content/editorial/Acces_API_Entreprises>
3. In the personal account area, go to `Mes acces API / SFTP`.
4. Select the enterprise/RNE datasets and formats you need, accept the reuse licence, and submit the access request.
5. Wait for INPI to send the technical connection identifiers.
6. Run the script with the SFTP values:

```bash
INPI_SFTP_HOST=... \
INPI_SFTP_USER=... \
INPI_SFTP_PASS=... \
INPI_SFTP_REMOTE=/... \
./06-rne-inpi.sh
```

The script expects `lftp` because it uses `mirror --continue` for resumable SFTP downloads.

### ACPR REGAFI API access

The current script downloads public REFASSU XLSX files only. If REGAFI API ingestion is added later, get access from the Banque de France REGAFI developer portal.

1. Create an account on the REGAFI API developer portal:
   <https://developer.regafi.banque-france.fr/>
2. Open the REGAFI product page:
   <https://developer.regafi.banque-france.fr/product/1282>
3. Select the default plan and create/subscribe an application.
4. Retrieve the application credentials or subscription key from the portal's application area.
5. Store them outside git, for example in a local shell profile or `.env.local`, then wire the future REGAFI extraction script to those variables.

The portal currently advertises a default plan of 100 calls/hour and REGAFI FR/EN REST APIs.

### Optional Sirene API token

`02-sirene.sh` uses public monthly stock files and does not need a token. A token is only needed if you later add live Sirene API lookups or daily/API-based refreshes.

1. Sign in to the INSEE API portal:
   <https://portail-api.insee.fr/>
2. Create an application or open an existing application.
3. Subscribe that application to the Sirene API.
4. In the application access/key area, generate or retrieve the access token / consumer credentials according to the portal documentation.
5. Keep the secret outside git, for example:

```bash
export INSEE_API_TOKEN=...
```

The public API Sirene data.gouv page lists access as `Ouvert avec compte` and documents a 30 requests/minute open-data limit:
<https://www.data.gouv.fr/dataservices/api-sirene-open-data>

### No API key expected

These scripts are public-download or manifest-download based:

- `01-bofip.sh`
- `03-autorite-concurrence.sh`
- `04-info-financiere-amf.sh`
- `05-eurlex.sh`
- `08-ted.sh`
- `09-dg-comp-eu.sh`

`05-eurlex.sh` needs `EURLEX_URLS_FILE` only for full dump URL selection. `09-dg-comp-eu.sh` needs `DG_COMP_URLS_FILE` only so you can choose the exact data.europa.eu distributions to download.

All scripts are resumable where the source protocol supports it. Partial HTTP downloads are kept as `.part` files and resumed with `curl -C -`; INPI SFTP uses `lftp mirror --continue`.
