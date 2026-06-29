import type { NextConfig } from "next";

// NEXT_PUBLIC_GL_API_URL is baked into the client bundle at build time and drives every
// browser-side call in lib/api.ts. Enforce its presence here — this runs at build time
// on the Node side, so a missing value fails the build loudly instead of silently
// shipping a bundle whose fetches hit relative paths and 404 against the Next server.
// Mirrors the GL_CONFIG_API_URL hard requirement in lib/config.ts.
if (!process.env.NEXT_PUBLIC_GL_API_URL) {
  throw new Error(
    "NEXT_PUBLIC_GL_API_URL is not set. Point it at your gl-serv base URL " +
      "(e.g. http://localhost:3001) in .env.local or Vercel project settings.",
  );
}

const nextConfig: NextConfig = {
  /* config options here */
};

export default nextConfig;
