import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { ImageResponse } from 'next/og';

// The social card: what Slack, X, iMessage, etc. show when someone shares a link.
// Next turns this file into the og:image tags and renders the PNG once, at build time.

export const alt = 'Goopy.life';
export const size = { width: 1200, height: 630 };
export const contentType = 'image/png';

export default async function OpengraphImage() {
	// The hero 💩 as it looks on the site, cropped from a screenshot: the card
	// renderer has no CSS filters to tint a glyph itself.
	const emoji = await readFile(join(process.cwd(), 'assets/goop-emoji.png'), 'base64');

	return new ImageResponse(
		(
			<div
				style={{
					width: '100%',
					height: '100%',
					display: 'flex',
					flexDirection: 'column',
					alignItems: 'center',
					justifyContent: 'center',
					background: '#000',
					color: '#ededed',
				}}
			>
				<img src={`data:image/png;base64,${emoji}`} width={344} height={340} alt="" />
				<div style={{ fontSize: 72, marginTop: 8 }}>Goopy.life</div>
				<div style={{ fontSize: 36, marginTop: 12, color: '#999' }}>
					Spin up a throwaway Ghost site in one click.
				</div>
			</div>
		),
		size,
	);
}
