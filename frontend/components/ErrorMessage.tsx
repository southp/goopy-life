import IssueLink from "@/components/IssueLink";
import { NO_FREE_SLOT_MESSAGE } from "@/lib/constants";
import { isExpectedError, type ExpectedErrorCode } from "@/lib/errors";

interface ErrorMessageProps {
	message: string;
	/** The API's error code, or null when the response carried none. */
	code?: string | null;
	onReset: () => void;
}

/**
 * Copy for the conditions the service produces by design (lib/errors.ts).
 *
 * These do not fall through to the server's own message: it is written for a
 * log (`server is full; no capacity for new instances`), and it names
 * internals a visitor has no use for. An expected code also drops the "file an
 * issue" prompt — nothing is broken, and inviting a report trains people to
 * file noise.
 *
 * Anything else keeps the server's message and stays reportable. Keyed by the
 * full set of expected codes, so a new one cannot ship without its copy.
 */
const EXPECTED_ERRORS: Record<ExpectedErrorCode, string> = {
	server_full: NO_FREE_SLOT_MESSAGE,
	server_busy: NO_FREE_SLOT_MESSAGE,
	too_many_requests: "That was a lot of tries at once. Wait a moment, then try again.",
};

export default function ErrorMessage({
	message,
	code = null,
	onReset,
}: ErrorMessageProps) {
	const expected = isExpectedError(code) ? EXPECTED_ERRORS[code] : undefined;

	return (
		<div className="error-block">
			<p className={`error-message${expected ? " expected" : ""}`}>
				{expected ?? message}
			</p>
			{!expected && <IssueLink prompt="Need help?" />}
			<button className="go-button idle" onClick={onReset}>
				Try again
			</button>
		</div>
	);
}
