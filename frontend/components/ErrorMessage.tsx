import IssueLink from "@/components/IssueLink";
import { NO_FREE_SLOT_MESSAGE } from "@/lib/constants";

interface ErrorMessageProps {
	message: string;
	/** The API's error code, or null when the response carried none. */
	code?: string | null;
	onReset: () => void;
}

/**
 * Copy for the conditions the service produces by design, keyed by API error
 * code.
 *
 * Two reasons these do not fall through to the server's own message: it is
 * written for a log (`server is full; no capacity for new instances`), and it
 * names internals a visitor has no use for. A code listed here also drops the
 * "file an issue" prompt — nothing is broken, and inviting a report trains
 * people to file noise.
 *
 * Anything not listed keeps the server's message and stays reportable, so a
 * genuine fault is never swallowed by friendly copy.
 */
const EXPECTED_ERRORS: Record<string, string> = {
	server_full: NO_FREE_SLOT_MESSAGE,
	server_busy: NO_FREE_SLOT_MESSAGE,
	too_many_requests: "That was a lot of tries at once. Wait a moment, then try again.",
};

export default function ErrorMessage({
	message,
	code = null,
	onReset,
}: ErrorMessageProps) {
	const expected = code === null ? undefined : EXPECTED_ERRORS[code];

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
