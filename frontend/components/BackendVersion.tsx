"use client";

import { useEffect, useState } from "react";
import { getVersion } from "@/lib/api";
import { GITHUB_REPO_URL } from "@/lib/constants";
import { describeSha } from "@/lib/version";
import ShaLink from "@/components/ShaLink";

/**
 * The backend half of the version footer: the commit gl-serv is running, read
 * from `GET /version` once on load.
 *
 * Renders nothing until it answers, and nothing at all if it never does — an
 * unreachable `/version` must not become a placeholder that implies a version.
 * `unknown` and `-dirty` are real answers from the server and are shown as such.
 */
export default function BackendVersion() {
	const [sha, setSha] = useState<string | null>(null);

	useEffect(() => {
		const controller = new AbortController();
		getVersion(controller.signal)
			.then((v) => {
				if (typeof v.sha_full === "string") {
					setSha(v.sha_full);
				}
			})
			.catch(() => {
				// Swallowed on purpose: omitting the backend half is the fallback.
			});
		return () => {
			controller.abort();
		};
	}, []);

	if (sha === null) {
		return null;
	}

	return (
		<>
			{" · "}
			<ShaLink name="api" display={describeSha(sha, GITHUB_REPO_URL)} />
		</>
	);
}
