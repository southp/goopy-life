// Core data definitions shared across the frontend.

export type GoopyStatus =
	| "Spawning"
	| "Done"
	| "Failed"
	| "Despawning"
	| "Archived"
	| "Empty";

export interface GoopyResponse {
	slug: string;
	status: GoopyStatus;
	url: string;
	created_at: string;
	expires_at: string;
	is_expired: boolean;
}

export interface ApiError {
	error: string;
	code: string;
}

// Runtime config served by gl-serv at `GET /config`.
// Values are nullable: when the build-time fetch fails they fall back to `null`
// so the UI can render placeholders instead of fabricated numbers.
export interface ConfigResponse {
	life_in_days: number | null;
	storage_quota_mb: number | null;
}

// State machine for the interactive "Ghost now!" CTA.
export type AppState =
	| { kind: "idle" }
	| { kind: "resuming"; slug: string }
	| { kind: "spawning" }
	| { kind: "done"; slug: string; url: string }
	| { kind: "expired"; slug: string }
	| { kind: "failed"; slug: string }
	| { kind: "error"; message: string };
