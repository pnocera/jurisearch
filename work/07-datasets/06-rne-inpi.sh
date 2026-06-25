#!/usr/bin/env bash
set -euo pipefail

DEST_ROOT="${DEST_ROOT:-/mnt/models/opendata}"
DEST="$DEST_ROOT/RNE_INPI"
INPI_TRANSFER_SCHEME="${INPI_TRANSFER_SCHEME:-ftp}"
case "$INPI_TRANSFER_SCHEME" in
  ftp)
    INPI_SFTP_HOST="${INPI_SFTP_HOST:-www.inpi.net}"
    INPI_SFTP_PORT="${INPI_SFTP_PORT:-21}"
    ;;
  sftp)
    INPI_SFTP_HOST="${INPI_SFTP_HOST:-registre-national-entreprises.inpi.fr}"
    INPI_SFTP_PORT="${INPI_SFTP_PORT:-9222}"
    ;;
  *)
    echo "Unsupported INPI_TRANSFER_SCHEME=$INPI_TRANSFER_SCHEME; use ftp or sftp." >&2
    exit 1
    ;;
esac
mkdir -p "$DEST"

cat > "$DEST/README.source.txt" <<'EOF'
Source: INPI Data, Registre national des entreprises.
Credentials are required for the full API/FTP/SFTP access. Create/enable access from
a Data INPI account, then run this script with:

  INPI_TRANSFER_SCHEME=ftp INPI_SFTP_HOST=www.inpi.net INPI_SFTP_USER=... INPI_SFTP_PASS=... INPI_SFTP_REMOTE=/ ./06-rne-inpi.sh

The script uses lftp mirror --continue for resumable downloads.
Default mode follows the current Data INPI personal-space URL: FTP on www.inpi.net.
Set INPI_TRANSFER_SCHEME=sftp for INPI's SFTP host/port if your account uses SFTP.

Access page:
https://data.inpi.fr/content/editorial/Acces_API_Entreprises
EOF

if [[ -z "${INPI_SFTP_HOST:-}" || -z "${INPI_SFTP_USER:-}" || -z "${INPI_SFTP_REMOTE:-}" ]]; then
  echo "INPI FTP/SFTP settings missing. See $DEST/README.source.txt"
  exit 0
fi

if ! command -v lftp >/dev/null 2>&1; then
  echo "lftp is required for resumable FTP/SFTP mirroring. Install it, then rerun."
  exit 1
fi

echo "Checking INPI $INPI_TRANSFER_SCHEME login and remote path..."
if ! lftp -u "$INPI_SFTP_USER","${INPI_SFTP_PASS:-}" "$INPI_TRANSFER_SCHEME://$INPI_SFTP_HOST:$INPI_SFTP_PORT" <<EOF
set sftp:auto-confirm yes
set net:timeout 20
set net:max-retries 1
cls -la "$INPI_SFTP_REMOTE"
bye
EOF
then
  cat >&2 <<EOF
INPI FTP/SFTP preflight failed.

Check:
- Your Data INPI personal-space link may be ftp://...@www.inpi.net/.
- For that link, use INPI_TRANSFER_SCHEME=ftp, INPI_SFTP_HOST=www.inpi.net, INPI_SFTP_PORT=21.
- For SFTP technical-doc access, use INPI_TRANSFER_SCHEME=sftp, INPI_SFTP_HOST=registre-national-entreprises.inpi.fr, INPI_SFTP_PORT=9222.
- INPI_SFTP_USER and INPI_SFTP_PASS are the technical FTP/SFTP credentials, not necessarily the Data INPI web login.
- INPI_SFTP_REMOTE exists for your account. Use "/" to list the account root.
- Your IP/network can reach the INPI FTP/SFTP service.
EOF
  exit 1
fi

lftp -u "$INPI_SFTP_USER","${INPI_SFTP_PASS:-}" "$INPI_TRANSFER_SCHEME://$INPI_SFTP_HOST:$INPI_SFTP_PORT" <<EOF
set sftp:auto-confirm yes
set net:timeout 30
set net:max-retries 8
mirror --continue --verbose "$INPI_SFTP_REMOTE" "$DEST"
bye
EOF
