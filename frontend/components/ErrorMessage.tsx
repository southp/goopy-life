import { GITHUB_ISSUES_URL } from "@/lib/constants";

interface ErrorMessageProps {
	message: string;
	onReset: () => void;
}

export default function ErrorMessage({ message, onReset }: ErrorMessageProps) {
	return (
		<div className="error-block">
			<p className="error-message">{message}</p>
			<p>
				Need help?{" "}
				<a
					href={GITHUB_ISSUES_URL}
					className="error-link"
					target="_blank"
					rel="noopener noreferrer"
				>
					Open an issue on GitHub
				</a>
			</p>
			<button className="go-button idle" onClick={onReset}>
				Try again
			</button>
		</div>
	);
}
