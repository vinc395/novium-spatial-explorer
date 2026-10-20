// NOVIUM Spatial Explorer at a fixed workers.dev address. Every request goes through
// Workers VPC and the "novium" Cloudflare Tunnel to the public explorer instance on the
// workstation (127.0.0.1:8444, `scripts/public_tunnel.sh`). Sign-in, sessions and
// lockouts stay in the explorer; the visitor's address is passed on for its per-visitor
// lockout. The tunnel has no public hostname, so this Worker is the only way in.
const TARGET = "http://127.0.0.1:8444";

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const headers = new Headers(request.headers);
    const visitor = request.headers.get("cf-connecting-ip");
    if (visitor) {
      headers.set("cf-connecting-ip", visitor);
      headers.set("x-forwarded-for", visitor);
    }
    headers.set("x-forwarded-proto", "https");
    const init = { method: request.method, headers, redirect: "manual" };
    if (request.method !== "GET" && request.method !== "HEAD") init.body = request.body;
    try {
      return await env.EXPLORER.fetch(new Request(TARGET + url.pathname + url.search, init));
    } catch {
      return new Response("NOVIUM Spatial Explorer is offline right now. Please try again later.", {
        status: 503,
        headers: { "content-type": "text/plain; charset=utf-8", "cache-control": "no-store" },
      });
    }
  },
};
