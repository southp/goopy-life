"use client";

import { useEffect, useState } from 'react';

type Status = { kind: 'idle' }
	| { kind: 'busy' }
	| { kind: 'done'; url: string }
;

function GoButton({ status, onClick }: {status: Status, onClick: () => void }) {
	const [countDown, setCountDown] = useState<number>(3);
	useEffect(() => {
		if (status.kind !== 'done'){
			return;
		}
		if (countDown === 0) {
			location.assign(status.url);
			return;
		}

		const id = setTimeout(() => setCountDown(countDown - 1), 1000);
		return () => clearTimeout(id);
	}, [countDown, status.kind]);

	if (status.kind === 'idle') {
		return (
			<button className="go-button idle" onClick={ onClick }>
				Ghost now!
			</button>
		);
	}

	if (status.kind === 'busy') {
		return (
			<span className="spinner"></span>
		);
	}

	if (status.kind === 'done') {
		return (
			<div className="go-button-done-message">
				<p> Your Ghost instance is ready at:</p>
				<a className="go-button-url" href={status.url} >
					https://foo.goopy.life
				</a>
				<p className="go-button-countdown">Redirecting in {countDown}...</p>
			</div>
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
				url: "https://bar.southp.dev/ghost/",

			} );
		}, 3000);
	}

	return (
		<div className="page-wrapper">
			<main className="page-main">
				<span className="hero-emoji">💩</span>
				<div className="cta-area">
					<GoButton status={ status } onClick={ onClick } />
				</div>
			</main>
		</div>
	);
}
