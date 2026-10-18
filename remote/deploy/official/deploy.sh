#!/bin/sh
# Builds and installs the relay on the official server, mad-gamer.com, from
# this checkout. Only for the official instance; self-hosters follow
# docs/remote-access.md with the files in deploy/systemd and deploy/nginx.
#
#   sh remote/deploy/official/deploy.sh [user@host]
#
# Needs ssh access with sudo on the server, and Rust (cargo) there.
set -eu
TARGET=${1:-matthew@mad-gamer.com}
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REMOTE_DIR=$(CDPATH= cd -- "$HERE/../.." && pwd)

echo "Packing the relay source"
tar -C "$REMOTE_DIR" --exclude=target --exclude=ios -czf - . | ssh "$TARGET" '
  set -eu
  rm -rf ~/backtalk-remote-src && mkdir -p ~/backtalk-remote-src
  tar -C ~/backtalk-remote-src -xzf -
'

echo "Building and installing on $TARGET"
ssh "$TARGET" '
  set -eu
  cd ~/backtalk-remote-src
  ~/.cargo/bin/cargo build --release --locked -p backtalk-remote-relay
  sudo install -D -m 0755 target/release/backtalk-remote-relay /opt/backtalk-remote/bin/backtalk-remote-relay
  sudo install -D -m 0644 deploy/systemd/backtalk-remote-relay.service /etc/systemd/system/backtalk-remote-relay.service
  sudo install -D -m 0644 deploy/nginx/backtalk-remote.conf /etc/nginx/snippets/backtalk-remote.conf
  # Include the snippet in the HTTPS server block of mad-gamer.com, once.
  SITES=/etc/nginx/sites-available/sites.conf
  if ! grep -q "snippets/backtalk-remote.conf" "$SITES"; then
    sudo cp "$SITES" "$SITES.before-backtalk-remote"
    sudo python3 - "$SITES" <<PY
import sys
path = sys.argv[1]
text = open(path).read()
anchor = (
    "    server_name mad-gamer.com www.mad-gamer.com;\n"
    "    include /etc/nginx/snippets/ssl-wildcard.conf;\n"
)
assert text.count(anchor) == 1, "the mad-gamer.com HTTPS block was not found once"
text = text.replace(anchor, anchor + "    include /etc/nginx/snippets/backtalk-remote.conf;\n")
open(path, "w").write(text)
PY
  fi
  sudo nginx -t
  sudo systemctl daemon-reload
  sudo systemctl enable backtalk-remote-relay
  sudo systemctl restart backtalk-remote-relay
  sudo systemctl reload nginx
  sleep 1
  systemctl is-active backtalk-remote-relay
'

echo "Checking the relay through TLS"
curl -fsS https://mad-gamer.com/backtalk-remote/health
