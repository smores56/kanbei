/**
 * The static shell's CSP (`07 §4.11.7`). The shell and every response carry it;
 * a test asserts the exact string.
 */
export const CSP_DIRECTIVES = [
  "default-src 'none'",
  "script-src 'self'",
  "style-src 'self'",
  "img-src 'self' data:",
  "connect-src 'self'",
  "form-action 'self'",
  "frame-ancestors 'none'",
  "base-uri 'none'",
] as const

export const CONTENT_SECURITY_POLICY = CSP_DIRECTIVES.join("; ")

export const CONTENT_SECURITY_POLICY_HEADER = "Content-Security-Policy"

export const shellResponseHeaders: Readonly<Record<string, string>> = {
  [CONTENT_SECURITY_POLICY_HEADER]: CONTENT_SECURITY_POLICY,
  "Content-Type": "text/plain; charset=utf-8",
  "X-Content-Type-Options": "nosniff",
  "Referrer-Policy": "no-referrer",
}

/** Only the embedded `data:` image is a resource; every other origin is blocked. */
export const ALLOWED_RESOURCE_SCHEMES = ["self", "data:"] as const
