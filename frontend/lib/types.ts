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

// Live capacity served by gl-serv at `GET /capacity`.
// Kept separate from ConfigResponse because it is fetched from the browser on an
// interval: the caps are static, but how much of them is used is not, and the
// build-time config fetch would freeze that at deploy.
export interface CapacityResponse {
	active: number;
	max_active: number;
	provisioned: number;
	max_provisioned: number;
	// Whether either cap is met. Computed server-side so the UI cannot drift from
	// the server's own definition of "full".
	is_full: boolean;
}

// State machine for the interactive "Ghost now!" CTA.
export type AppState =
	| { kind: "idle" }
	| { kind: "resuming"; slug: string }
	| { kind: "spawning" }
	| { kind: "done"; slug: string; url: string }
	| { kind: "expired"; slug: string }
	| { kind: "failed"; slug: string }
	// `code` is the API's error code (or null when the response carried none); it
	// distinguishes expected conditions, such as a full server, from real faults.
	| { kind: "error"; message: string; code: string | null };
