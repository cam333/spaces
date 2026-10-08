import type { NextConfig } from "next";

const basePath = process.env.SPACES_BASE_PATH || '';

const nextConfig: NextConfig = {
  reactStrictMode: false,
  output: process.env.STANDALONE === '1' ? 'standalone' : undefined,
  basePath: basePath || undefined,
  trailingSlash: true,
  // Inline SPACES_BASE_PATH into the client bundle so api() can use it
  env: { SPACES_BASE_PATH: basePath },
  serverExternalPackages: ['better-sqlite3', 'node-pty', 'ws', 'chokidar', '@spaces/pro', '@spaces/teams'],
  // Add your dev hostname via SPACES_DEV_ORIGIN if running Next dev behind a non-localhost proxy.
  allowedDevOrigins: process.env.SPACES_DEV_ORIGIN ? [process.env.SPACES_DEV_ORIGIN] : undefined,
  devIndicators: false,
  turbopack: {
    root: process.cwd(),
  },
  // Nothing uses next/image, so there is no optimizer to feed — and without
  // this, the standalone build would ship sharp plus the build machine's
  // platform-specific libvips binaries (~10 MB) to every install.
  images: { unoptimized: true },
  // A few routes touch the filesystem with paths the tracer cannot resolve
  // statically (api/files, api/whisper), so it conservatively traces the
  // whole project into .next/standalone — tests, docs, the Rust client
  // (including a local tui/target, which can be gigabytes), scripts, the
  // lockfile, built platform packages under npm/. None of it is needed at
  // runtime. bin/scrub-standalone.js deletes the same list at prepack, in
  // case anything slips past this.
  outputFileTracingExcludes: {
    '*': [
      // Sources: the server runs the compiled .next output; bin/ runs from
      // the package root, never from inside standalone.
      './src/**',
      './bin/**',
      './tests/**',
      './docs/**',
      './tui/**',
      './scripts/**',
      './npm/**',
      './coverage/**',
      './.github/**',
      './*.md',
      './*.gif',
      './public/*.gif',
      './nginx.conf',
      './docker-compose.yml',
      './package-lock.json',
      './tsconfig.tsbuildinfo',
      './vitest.config.ts',
      './eslint.config.mjs',
      './rust-toolchain.toml',
      './node_modules/sharp/**',
      './node_modules/@img/**',
    ],
  },
  // SECURITY (review S2): baseline hardening headers on every response.
  // frame-ancestors 'self' (+ X-Frame-Options SAMEORIGIN) blocks cross-site
  // clickjacking while still allowing the app's own same-origin preview frames.
  // A strict script-src CSP is intentionally NOT set here — it needs nonce
  // wiring + UI testing and is tracked as a follow-up.
  async headers() {
    return [
      {
        source: '/:path*',
        headers: [
          { key: 'Content-Security-Policy', value: "frame-ancestors 'self'; base-uri 'self'; object-src 'none'" },
          { key: 'X-Frame-Options', value: 'SAMEORIGIN' },
          { key: 'X-Content-Type-Options', value: 'nosniff' },
          { key: 'Referrer-Policy', value: 'strict-origin-when-cross-origin' },
          { key: 'Strict-Transport-Security', value: 'max-age=15552000; includeSubDomains' },
        ],
      },
    ];
  },
};

export default nextConfig;
