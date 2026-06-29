import { GITHUB_ISSUES_URL } from "@/lib/constants";

interface IssueLinkProps {
	prompt: string;
}

// A "<prompt> Open an issue on GitHub" line, shared by the terminal failure states
// (error and failed) so both give the user a consistent path to get help.
export default function IssueLink({ prompt }: IssueLinkProps) {
	return (
		<p>
			{prompt}{" "}
			<a
				href={GITHUB_ISSUES_URL}
				className="error-link"
				target="_blank"
				rel="noopener noreferrer"
			>
				Open an issue on GitHub
			</a>
		</p>
	);
}
