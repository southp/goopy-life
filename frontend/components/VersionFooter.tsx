import BackendVersion from "@/components/BackendVersion";
import ShaLink from "@/components/ShaLink";
import { GITHUB_REPO_URL } from "@/lib/constants";
import { describeSha } from "@/lib/version";

/**
 * One line naming the commit each half is running: `web <sha> · api <sha>`.
 *
 * The two halves come from two places on purpose. The frontend sha is baked in
 * at build time, which is correct — this bundle *is* that build. The backend sha
 * is read from the browser at runtime by <BackendVersion />, because the page is
 * static and does not rebuild on a backend-only deploy.
 */
export default function VersionFooter() {
	return (
		<footer className="version-footer">
			<ShaLink
				name="web"
				display={describeSha(
					process.env.NEXT_PUBLIC_GL_BUILD_SHA,
					GITHUB_REPO_URL,
				)}
			/>
			<BackendVersion />
		</footer>
	);
}
