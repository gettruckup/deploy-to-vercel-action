'use strict'
// Test-only action: exercises dist/cli-install.cjs against the real GitHub cache service.
// Only actions (not `run:` steps) receive the cache-service credentials, hence a local action.
const fs = require('fs')
const path = require('path')
const { spawnSync } = require('child_process')

const VERSION = '48.0.0'
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
	if (!/(Installed|Restored) Vercel CLI 48\.0\.0/.test(first.output)) throw new Error('first call neither installed nor restored the CLI')

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
