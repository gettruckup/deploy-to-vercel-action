'use strict'
// Test-only action: exercises dist/cli-install.cjs against the real GitHub cache service.
// Only actions (not `run:` steps) receive the cache-service credentials, hence a local action.
const fs = require('fs')
const path = require('path')
const { spawnSync } = require('child_process')

const VERSION = '48.0.0'
// A cache key unique to this run attempt makes every CI run exercise install, save and restore,
// and keeps concurrent runs from racing for the same cache reservation.
process.env.ImageOS = `${ process.env.ImageOS || process.platform }-cache-smoke-${ process.env.GITHUB_RUN_ID }-${ process.env.GITHUB_RUN_ATTEMPT }`
const { ensureVercelCli, installFolder } = require(path.join(process.env.GITHUB_WORKSPACE, 'dist', 'cli-install.cjs'))

const captureStdout = async (fn) => {
	const original = process.stdout.write.bind(process.stdout)
	let output = ''
	process.stdout.write = (chunk, ...rest) => {
		output += chunk
		return original(chunk, ...rest)
	}
	try {
		return { binDir: await fn(), output }
	} finally {
		process.stdout.write = original
	}
}

const assertVersion = (binDir) => {
	const result = spawnSync(path.join(binDir, 'vercel'), [ '--version' ], { encoding: 'utf8' })
	const out = `${ result.stdout || '' }${ result.stderr || '' }`
	if (!out.includes(VERSION)) throw new Error(`vercel --version did not report ${ VERSION }: ${ out }`)
}

const run = async () => {
	const folder = installFolder(VERSION, process.env)
	fs.rmSync(folder, { recursive: true, force: true })
	const first = await captureStdout(() => ensureVercelCli(VERSION))
	assertVersion(first.binDir)
	if (!first.output.includes(`Installed Vercel CLI ${ VERSION } with npm`)) throw new Error('first call did not install the CLI with npm')
	if (!first.output.includes(`Saved Vercel CLI ${ VERSION } to cache`)) throw new Error('first call did not save the CLI to cache')

	fs.rmSync(folder, { recursive: true, force: true })
	const second = await captureStdout(() => ensureVercelCli(VERSION))
	assertVersion(second.binDir)
	if (!second.output.includes(`Restored Vercel CLI ${ VERSION } from cache`)) throw new Error('second call did not restore the CLI from cache')

	process.stdout.write('cache-smoke: install and cache restore round trip OK\n')
}

run().catch((err) => {
	process.stdout.write(`::error::${ err.message }\n`)
	process.exit(1)
})
