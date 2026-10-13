// Which API errors are the service working as designed.
//
// A full server or a rate limit is a refusal, not a fault: nothing is broken,
// so the UI words it as ordinary copy and does not invite a bug report. Any
// other code — or none — is treated as a genuine fault, so one is never
// swallowed by friendly handling.
//
// Deliberately import-free, so `node --test` can load it without Next's module
// resolution.

export const EXPECTED_ERROR_CODES = ["server_full", "server_busy", "too_many_requests"] as const;

export type ExpectedErrorCode = (typeof EXPECTED_ERROR_CODES)[number];

export function isExpectedError(code: string | null): code is ExpectedErrorCode {
	return (EXPECTED_ERROR_CODES as readonly (string | null)[]).includes(code);
}
