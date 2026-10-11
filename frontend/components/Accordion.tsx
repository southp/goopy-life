import type { ReactNode } from "react";

interface AccordionProps {
	title: string;
	children: ReactNode;
	// Initial state only — the browser owns toggling after that.
	defaultOpen?: boolean;
}

// A collapsible section built on the native <details>/<summary> elements. This keeps
// it a Server Component — no client JS, no hydration — so the content is present in
// the server-rendered HTML even while collapsed and the browser handles toggling.
// Renders collapsed unless `defaultOpen` is set.
export default function Accordion({ title, children, defaultOpen = false }: AccordionProps) {
	return (
		<details className="accordion" open={defaultOpen}>
			<summary className="accordion-summary">
				<span className="section-heading">{title}</span>
			</summary>
			<div className="accordion-body">{children}</div>
		</details>
	);
}
