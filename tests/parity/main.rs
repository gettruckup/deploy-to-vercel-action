//! Differential parity tests: the v1 Node bundle vs the v2 binary against the same fakes (spec §9.2).
//! `legacy/index.js` is `git show a8d4a64:dist/index.js`, unmodified; its Vercel URL is patched in a temp copy.

mod harness;
mod scenarios;
