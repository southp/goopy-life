export const dynamic = 'force-static';

import HomeClient from './home-client';

interface ConfigResponse {
	life_in_days: number;
	storage_quota_mb: number;
}

const CONFIG_FALLBACK: ConfigResponse = {
	life_in_days: 7,
	storage_quota_mb: 512,
};

async function fetchConfig(): Promise<ConfigResponse> {
	const apiUrl = process.env.API_URL ?? 'http://localhost:3001';
	try {
		const res = await fetch(`${apiUrl}/config`);
		if (!res.ok) return CONFIG_FALLBACK;
		return (await res.json()) as ConfigResponse;
	} catch {
		return CONFIG_FALLBACK;
	}
}

export default async function Home() {
	const config = await fetchConfig();

	return (
		<HomeClient
			lifeInDays={config.life_in_days}
			storageQuotaMb={config.storage_quota_mb}
		/>
	);
}
