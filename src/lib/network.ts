const PROBE_URL = "https://build.protomaps.com/";
const PROBE_TIMEOUT_MS = 4000;

/** Quick reachability probe against the same host the offline-map downloader
 * itself hits - a generic "is the internet up" check could pass while this
 * specific host is unreachable (or vice versa on a filtered network), so
 * this answers the question that actually matters: would a download attempt
 * work right now. `no-cors` mode avoids needing CORS headers from that host;
 * a settled (even opaque) response means the network round-trip succeeded,
 * which is all this needs to know - the body itself is never read. */
export async function checkInternetConnection(): Promise<boolean> {
  try {
    await fetch(PROBE_URL, { mode: "no-cors", signal: AbortSignal.timeout(PROBE_TIMEOUT_MS) });
    return true;
  } catch {
    return false;
  }
}
