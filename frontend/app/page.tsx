"use client";

import { useState } from 'react';

type Status = { kind: 'idle' }
	| { kind: 'busy' }
	| { kind: 'done'; url: string }
;

function GoButton({ status, onClick }: {status: Status, onClick: () => void }) {
	if (status.kind === 'idle') {
		return (
			<button className="ghost-button idle" onClick={onClick}>
				Ghost now!
			</button>
		);
	}

	if (status.kind === 'busy') {
		return (
			<p> Ohhhh ... I am busy ... </p>
		);
	}

	if (status.kind === 'done') {
		return (
			<>
				<p> Your Ghost instance is ready at: <br/>
					<a href={status.url} >
						https://foo.goopy.life
					</a>
				</p>
				<p> Redirect you in 5 ...</p>
			</>
		)
	}

}

export default function Home() {
	const [status, setStatus] = useState<Status>({ kind: 'idle' });

	const onClick = () => {
		setStatus({kind: "busy"});
		setTimeout(() => {
			setStatus( {
				kind: "done",
				url: "https://foo.southp.dev",

			} );
		}, 3000);
	}

	return (
		<div className="page-wrapper">
			<main className="page-main">
				<span className="hero-emoji">💩</span>
				<GoButton status={ status } onClick={ onClick } />
			</main>
		</div>
	);
}
