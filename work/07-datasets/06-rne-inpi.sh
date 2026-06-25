#!/usr/bin/env bash
set -euo pipefail

DEST_ROOT="${DEST_ROOT:-/mnt/models/opendata}"
DEST="$DEST_ROOT/RNE_INPI"
mkdir -p "$DEST"

cat > "$DEST/README.source.txt" <<'EOF'
Source: INPI Data, Registre national des entreprises.
Credentials are required for the full API/SFTP access. Create/enable access from
a Data INPI account, then run this script with:

  INPI_SFTP_HOST=... INPI_SFTP_USER=... INPI_SFTP_PASS=... INPI_SFTP_REMOTE=/... ./06-rne-inpi.sh

The script uses lftp mirror --continue for resumable SFTP downloads.

Access page:
https://data.inpi.fr/content/editorial/Acces_API_Entreprises
EOF

if [[ -z "${INPI_SFTP_HOST:-}" || -z "${INPI_SFTP_USER:-}" || -z "${INPI_SFTP_REMOTE:-}" ]]; then
  echo "INPI SFTP settings missing. See $DEST/README.source.txt"
  exit 0
fi

if ! command -v lftp >/dev/null 2>&1; then
  echo "lftp is required for resumable SFTP mirroring. Install it, then rerun."
  exit 1
fi

lftp -u "$INPI_SFTP_USER","${INPI_SFTP_PASS:-}" "sftp://$INPI_SFTP_HOST" <<EOF
set sftp:auto-confirm yes
set net:timeout 30
set net:max-retries 8
mirror --continue --verbose "$INPI_SFTP_REMOTE" "$DEST"
bye
EOF

