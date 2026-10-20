# Deployment

The examples use `192.168.1.20` for the serving computer's LAN address and
`/data/xenium-web-index` for the index root; replace them with your own.

## Build
```bash
cargo build --release -p xenium-cli          # -> target/release/xenium-web
(cd web && npm ci && npm run build)          # -> web/dist
```
The binary is linked against the build machine's glibc. For an older HPC cluster, run
`scripts/build_portable.sh`: it produces `dist/xenium-web-x86_64-glibc2.17` (zig as the
C toolchain; needs only GLIBC ≥ 2.17), which you copy to the cluster. Building on the
cluster with `rustup` also works.

## Serve (single process: UI + API)
```bash
xenium-web serve --index-root /data/xenium-web-index --static-dir web/dist --port 8080
curl -fsS http://127.0.0.1:8080/healthz
```
- Binds `127.0.0.1`. A non-loopback `--host` is refused unless `--allow-remote` is given
  together with an access token and TLS (below).
- Remote viewing without opening a port: `ssh -L 8080:127.0.0.1:8080 user@workstation`,
  then open `http://127.0.0.1:8080` on your own computer. The browser's GPU renders; the
  server needs no GPU.
- Re-ingesting a section while the server runs is safe: the server notices the new version
  on the next request.

## Sharing on the local network (HTTPS + access token)
Check first that serving these data on your network is covered by your ethics and
data-management approvals.
1. Open the port for your subnet only (admin, once), e.g. for `192.168.1.20/24`:
   `sudo ufw allow from 192.168.1.0/24 to any port 8443 proto tcp`
2. Optional password: put one line in `~/.config/xenium-web/password` (mode 600, at least
   8 characters). Start the server:
   `LAN_IP=192.168.1.20 INDEX_ROOT=/data/xenium-web-index scripts/serve_lan.sh`.
   It keeps or creates a self-signed certificate (`~/.config/xenium-web/tls/`, 397 days,
   renewed on a restart near expiry) and a random 64-hex access token
   (`~/.config/xenium-web/token`, mode 600), and prints the private share link
   `https://192.168.1.20:8443/?token=…` (only to a terminal).
3. Colleagues open `https://192.168.1.20:8443`. The browser warns once that the
   certificate is self-signed; the connection is still encrypted. They use the token link
   or type the password on the sign-in page (sent in the request body, never in the
   address). Both set an HttpOnly, Secure, SameSite=Strict cookie for 30 days; "Sign out"
   (top right) drops it. Ten wrong attempts lock that computer out for 15 minutes. The
   API returns 401 without the cookie or a bearer token. `/healthz` stays open.
4. To revoke access, delete the token file (and change the password file if used),
   restart, and share the new link or password.
5. Optional sample information in the Index panel (e.g. age, sex, timepoint): put a CSV
   at `~/.config/xenium-web/sample_metadata.csv` (mode 600) and the columns to show,
   comma-separated, in `~/.config/xenium-web/sample_metadata_columns`. `serve_lan.sh`
   passes `--sample-metadata`, `--sample-metadata-id` (default `sample_id`, matched to
   section IDs ignoring case) and `--sample-metadata-columns`; only those columns are read
   and served (after sign-in), and values are never logged.
6. Colleagues on a VPN: allow the VPN client address range to port 8443 in the firewall
   as in step 1 (and check that your network routes their replies).

`serve` refuses a non-loopback address unless `--allow-remote`, `--access-token-file` and a
TLS certificate are all given (plain HTTP needs the explicit `--allow-plain-http`);
`--access-password-file` adds the password. Optional: `xenium-web gen-ca` /
`gen-cert --ca-dir` issue a certificate from a local, name-constrained CA that colleagues
trust once, which removes the browser warning.

## Always on (systemd user service, no sudo)
`LAN_IP=192.168.1.20 INDEX_ROOT=/data/xenium-web-index scripts/install_service.sh` copies
the release binary, `web/dist` and `serve_lan.sh` to `~/.local/share/novium`, writes the
user unit `novium-explorer.service` (`Restart=always`, retried every 15 s, so it also waits
for an index drive and the network after a boot) and runs `loginctl enable-linger` so it
starts at boot without anyone logging in. Rebuilding the repository does not touch the
running copy: rerun the script to deploy a new build. `--link` prints the private link;
`--uninstall` removes the service. Log: `~/.local/state/novium/server.log`.

## Public address (optional; Cloudflare, per-person sign-in)
Only with approval for serving your data on the internet through a third party:
Cloudflare decrypts the traffic at its edge.
- Access list: `scripts/public_tunnel.sh add-user EMAIL` puts someone on
  `~/.config/xenium-web/public/users.tsv` (mode 600, PBKDF2-HMAC-SHA256 hashes only).
  With a master password in `~/.config/xenium-web/public/master-password` they get that
  password; otherwise a password is generated and printed once. `remove-user` takes
  someone off (their session ends at their next request); `list-users` lists them;
  `set-master` changes the master password for everyone.
- `scripts/public_tunnel.sh start` starts a second explorer on `127.0.0.1:8444`
  (`serve --access-users-file … --behind-cloudflare`, without `--sample-metadata`, with
  its own UCell state) and a Cloudflare quick tunnel to it, and prints the random
  `https://….trycloudflare.com` address. `stop` turns public access off. Quick tunnels
  are for testing (no uptime guarantee; the address changes on restart).
- Fixed address on your own domain: `cloudflared tunnel login`, then
  `scripts/public_tunnel.sh named explorer.example.org`; `boot on` starts it at boot.
- Fixed `*.workers.dev` address without a domain: deploy the Worker in
  `deploy/cloudflare`, which forwards every request through Workers VPC to the tunnel
  (the tunnel has no public hostname, so the Worker is the only way in). With wrangler
  signed in (`npx wrangler login` in `deploy/cloudflare`):
  `npx wrangler tunnel create novium`;
  `npx wrangler vpc service create novium-explorer --type http --tunnel-id <id> --ipv4 127.0.0.1 --http-port 8444`;
  put the service id into `wrangler.jsonc`; `npx wrangler deploy`. Save the tunnel token
  with `scripts/public_tunnel.sh set-token`, then
  `scripts/public_tunnel.sh worker https://novium.<account>.workers.dev` and `boot on`.
- The public sign-in page asks for email and password; token links and the shared
  password are not accepted there. Wrong attempts lock out per visitor
  (`CF-Connecting-IP`, trusted only because `--behind-cloudflare` binds loopback).

## Ingesting on an HPC cluster (data stays on the cluster)
The indexer streams Parquet. Peak RAM is about one external-sort bucket plus the cell
table: 343 MB for 120k cells and 12.6M transcripts. For about 300M transcripts, allow
8–16 GB and fast scratch disk (temporary files are about 1.25× the final transcript
index). `scripts/slurm_ingest.sbatch` is a Slurm template: run `xenium-web inspect`
first, then submit one job per section. Copying an index elsewhere moves the
(patient-derived) data it contains, so check that it is permitted first.

## Online demo (GitHub Pages)
The demo runs without a server: `scripts/build_demo_site.sh` ingests two synthetic
sections, serves them locally, saves every reply the viewer needs as files
(`scripts/export_demo.ts`, paths from `web/src/demoPaths.ts`; binary replies gzipped)
and builds the viewer in demo mode (`VITE_DEMO=1`) into `site/`. Any static host can serve
`site/`; for GitHub Pages, publish it as the root of a `gh-pages` branch:
```bash
DEMO_REPO=https://github.com/<you>/<repo> scripts/build_demo_site.sh
cd site && git init -q -b gh-pages && git add -A && git commit -qm "Online demo" \
  && git push --force https://github.com/<you>/<repo>.git gh-pages
(cd web && DEMO_URL=https://<you>.github.io/<repo>/ npx playwright test -c playwright.demo.config.ts)
```
In the demo, gene
signature scoring and the server's CSV export are switched off (they need the server), and
transcript density uses QV 20. **Never build a demo from real data:** every reply becomes a
public file.

## Storage notes
A section index is a few dozen files (image tiles and cell fields are packed), which
suits network and USB drives. On a USB NTFS drive under Linux, avoid deleting old index
versions (`--keep-versions`) while servers read from the same drive.

## Security checklist
- Loopback bind by default; no CORS; CSP, `nosniff` and `no-referrer` headers on every
  response.
- IDs are validated and resolved through the manifest; the section path is canonicalised
  and must stay inside the index root.
- Request sizes are bounded (≤ 64 genes, ≤ 4M points per tile, ≤ 32 MB export body,
  ≤ 2M exported cells). Disk reads run behind a 32-slot limit; when all slots stay busy
  for 20 s (a stalled disk), requests get 503 instead of piling up threads.
- Error bodies are generic; details (which may include paths) go to the server log only.
- The index excludes run, region and slide identifiers from `experiment.xenium`. It does
  contain cell IDs, coordinates and H&E tiles, so treat it with the same care as the
  source data.
