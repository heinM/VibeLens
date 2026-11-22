#!/usr/bin/env bash
# Test-only helper for tests/conn_tunnel.rs. Starts (or stops) a throwaway sshd on 127.0.0.1.
#
#   conn_tunnel_setup.sh start <workdir>   install openssh-server if missing, generate keys and
#                                          config under <workdir>, start `sshd -D` in the
#                                          background, print KEY=VALUE lines on stdout
#   conn_tunnel_setup.sh stop  <workdir>   kill that sshd (safe to call repeatedly)
#
# Everything lives in <workdir>; the only system change is the optional `vgtest` user created
# (as root only) to exercise password authentication. `stop` removes it again.
set -euo pipefail

cmd="${1:-}"
wd="${2:-}"
port="${VG_TEST_SSH_PORT:-2222}"
pw_user="vgtest"
pw_pass="vg-test-password"

[ -n "$cmd" ] && [ -n "$wd" ] || { echo "usage: $0 start|stop <workdir>" >&2; exit 2; }

stop() {
  if [ -f "$wd/sshd.pid" ]; then
    pid="$(cat "$wd/sshd.pid" 2>/dev/null || true)"
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
      for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 "$pid" 2>/dev/null || break; sleep 0.2; done
      kill -9 "$pid" 2>/dev/null || true
    fi
    rm -f "$wd/sshd.pid"
  fi
  if [ -f "$wd/created_user" ] && [ "$(id -u)" = 0 ]; then
    userdel -r "$pw_user" >/dev/null 2>&1 || userdel "$pw_user" >/dev/null 2>&1 || true
    rm -f "$wd/created_user"
  fi
}

if [ "$cmd" = stop ]; then
  stop
  exit 0
fi
[ "$cmd" = start ] || { echo "unknown command: $cmd" >&2; exit 2; }

if ! command -v sshd >/dev/null 2>&1 && [ ! -x /usr/sbin/sshd ]; then
  echo "installing openssh-server..." >&2
  if [ "$(id -u)" = 0 ]; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y openssh-server >&2
  else
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y openssh-server >&2
  fi
fi
sshd_bin="$(command -v sshd || echo /usr/sbin/sshd)"

mkdir -p "$wd"
chmod 700 "$wd"
stop # a leftover instance from a crashed run

# sshd's privilege separation directory.
if [ "$(id -u)" = 0 ]; then mkdir -p /run/sshd && chmod 755 /run/sshd; fi

# Keys: host key, a plain client key, a passphrase-protected client key, and a second host key
# (used by the tests as "the wrong server").
rm -f "$wd"/host_ed25519* "$wd"/other_host_ed25519* "$wd"/client_ed25519* "$wd"/client_enc_ed25519*
ssh-keygen -q -t ed25519 -N "" -C vg-test-host   -f "$wd/host_ed25519"
ssh-keygen -q -t ed25519 -N "" -C vg-test-other  -f "$wd/other_host_ed25519"
ssh-keygen -q -t ed25519 -N "" -C vg-test-client -f "$wd/client_ed25519"
ssh-keygen -q -t ed25519 -N "vg-key-passphrase" -C vg-test-client-enc -f "$wd/client_enc_ed25519"
cat "$wd/client_ed25519.pub" "$wd/client_enc_ed25519.pub" > "$wd/authorized_keys"
chmod 600 "$wd"/host_ed25519 "$wd"/other_host_ed25519 "$wd"/client_ed25519 "$wd"/client_enc_ed25519 "$wd/authorized_keys"
chmod 755 "$wd"

login_user="$(id -un)"
have_pw_user=0
if [ "$(id -u)" = 0 ]; then
  if ! id "$pw_user" >/dev/null 2>&1; then
    if useradd -m -s /bin/bash "$pw_user" >/dev/null 2>&1; then
      echo "$wd" > "$wd/created_user"
    fi
  fi
  if id "$pw_user" >/dev/null 2>&1 && echo "$pw_user:$pw_pass" | chpasswd >/dev/null 2>&1; then
    have_pw_user=1
  fi
fi

cat > "$wd/sshd_config" <<CONF
Port $port
ListenAddress 127.0.0.1
HostKey $wd/host_ed25519
PidFile $wd/sshd.pid
AuthorizedKeysFile $wd/authorized_keys
StrictModes no
UsePAM no
PubkeyAuthentication yes
PasswordAuthentication yes
KbdInteractiveAuthentication no
PermitRootLogin yes
AllowTcpForwarding yes
PermitTunnel no
X11Forwarding no
# DEBUG1 logs every authentication request, which the tests read to prove that none was made.
LogLevel DEBUG1
CONF

"$sshd_bin" -t -f "$wd/sshd_config" >&2

# -D: stay in the foreground (so the pid we record is the daemon); backgrounded by the shell.
nohup "$sshd_bin" -D -e -f "$wd/sshd_config" > "$wd/sshd.log" 2>&1 &
echo $! > "$wd/sshd.pid"

# Wait until it accepts connections.
for _ in $(seq 1 50); do
  if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then break; fi
  sleep 0.1
done
if ! (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
  echo "sshd did not start; log:" >&2
  cat "$wd/sshd.log" >&2 || true
  exit 1
fi

fp="$(ssh-keygen -lf "$wd/host_ed25519.pub" -E sha256 | awk '{print $2}')"
other_fp="$(ssh-keygen -lf "$wd/other_host_ed25519.pub" -E sha256 | awk '{print $2}')"
cat <<OUT
SSH_PORT=$port
SSH_USER=$login_user
HOST_FINGERPRINT=$fp
OTHER_HOST_FINGERPRINT=$other_fp
CLIENT_KEY=$wd/client_ed25519
CLIENT_KEY_ENCRYPTED=$wd/client_enc_ed25519
CLIENT_KEY_PASSPHRASE=vg-key-passphrase
PASSWORD_USER=$([ "$have_pw_user" = 1 ] && echo "$pw_user" || echo "")
PASSWORD=$pw_pass
OUT
