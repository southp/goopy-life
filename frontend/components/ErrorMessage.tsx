import IssueLink from "@/components/IssueLink";

interface ErrorMessageProps {
	message: string;
	/** The API's error code, or null when the response carried none. */
	code?: string | null;
	onReset: () => void;
}

/**
 * Error codes that describe the service working as designed — a cap was hit, a
 * rate limit kicked in — rather than something broken. They still surface the
 * server's message, but without the "file a GitHub issue" prompt: there is
 * nothing to report, and inviting a report trains users to file noise.
 */
const EXPECTED_ERROR_CODES = new Set(["server_full", "server_busy", "too_many_requests"]);

export default function ErrorMessage({
	message,
	code = null,
	onReset,
}: ErrorMessageProps) {
	const reportable = code === null || !EXPECTED_ERROR_CODES.has(code);

	return (
		<div className="error-block">
			<p className={`error-message${reportable ? "" : " expected"}`}>{message}</p>
			{reportable && <IssueLink prompt="Need help?" />}
			<button className="go-button idle" onClick={onReset}>
				Try again
			</button>
		</div>
	);
}
