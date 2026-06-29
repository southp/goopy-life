import IssueLink from "@/components/IssueLink";

interface ErrorMessageProps {
	message: string;
	onReset: () => void;
}

export default function ErrorMessage({ message, onReset }: ErrorMessageProps) {
	return (
		<div className="error-block">
			<p className="error-message">{message}</p>
			<IssueLink prompt="Need help?" />
			<button className="go-button idle" onClick={onReset}>
				Try again
			</button>
		</div>
	);
}
