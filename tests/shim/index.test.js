'use strict'
const test = require('node:test')
const assert = require('node:assert')
const fs = require('fs')
const os = require('os')
const path = require('path')
const { spawnSync } = require('child_process')

const SHIM = path.join(__dirname, '..', '..', 'dist', 'index.js')
const { resolveBinary, resolveCliVersion, prependToPath, main } = require(SHIM)

const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), 'shim-'))
const LINUX = [ 'linux-x64', 'linux-arm64' ].includes(`${ process.platform }-${ process.arch }`)

test('rejects unsupported platforms', () => {
	assert.deepStrictEqual(resolveBinary('darwin', 'arm64', tmp()), {
		error: 'Unsupported runner platform darwin-arm64; supported: linux-x64, linux-arm64'
	})
})

test('reports a missing binary', () => {
	assert.deepStrictEqual(resolveBinary('linux', 'x64', tmp()), {
		error: 'Action binary not found; reference a release tag such as @v2'
	})
})

test('resolves the arm64 binary', () => {
	const dir = tmp()
	fs.mkdirSync(path.join(dir, 'bin'))
	const bin = path.join(dir, 'bin', 'deploy-to-vercel-aarch64-unknown-linux-musl')
	fs.writeFileSync(bin, '')
	assert.deepStrictEqual(resolveBinary('linux', 'arm64', dir), { bin })
})

test('execs the binary with the same env and exit code', { skip: !LINUX }, () => {
	const dist = path.join(tmp(), 'dist')
	fs.mkdirSync(path.join(dist, 'bin'), { recursive: true })
	fs.copyFileSync(SHIM, path.join(dist, 'index.js'))
	const target = process.arch === 'x64' ? 'x86_64-unknown-linux-musl' : 'aarch64-unknown-linux-musl'
	fs.writeFileSync(path.join(dist, 'bin', `deploy-to-vercel-${ target }`), '#!/bin/sh\necho "input=$INPUT_FOO"\nexit 7\n', { mode: 0o644 })
	const result = spawnSync(process.execPath, [ path.join(dist, 'index.js') ], { env: { ...process.env, INPUT_FOO: 'bar', INPUT_VERCEL_CLI_VERSION: 'false' }, encoding: 'utf8' })
	assert.strictEqual(result.status, 7)
	assert.strictEqual(result.stdout, 'input=bar\n')
})

const binDir = () => {
	const dir = tmp()
	fs.mkdirSync(path.join(dir, 'bin'))
	fs.writeFileSync(path.join(dir, 'bin', 'deploy-to-vercel-x86_64-unknown-linux-musl'), '')
	return dir
}

// Runs main() with fakes; records what reached the binary and what was printed.
const runMain = async (overrides = {}) => {
	const spawned = []
	const lines = []
	const ensured = []
	const code = await main({
		env: { PATH: '/usr/bin' },
		platform: 'linux',
		arch: 'x64',
		dir: binDir(),
		loadCliInstall: () => ({ ensureVercelCli: async (version) => { ensured.push(version); return '/cli/bin' } }),
		spawnSync: (bin, args, options) => { spawned.push(options.env); return { status: 7 } },
		write: (line) => lines.push(line),
		...overrides
	})
	return { code, spawned, lines, ensured }
}

test('resolves VERCEL_CLI_VERSION like the other inputs', () => {
	assert.deepStrictEqual(resolveCliVersion({}), { version: '48.0.0' })
	assert.deepStrictEqual(resolveCliVersion({ INPUT_VERCEL_CLI_VERSION: '49.1.2', VERCEL_CLI_VERSION: '47.0.0' }), { version: '49.1.2' })
	assert.deepStrictEqual(resolveCliVersion({ VERCEL_CLI_VERSION: '47.0.0' }), { version: '47.0.0' })
	assert.deepStrictEqual(resolveCliVersion({ INPUT_VERCEL_CLI_VERSION: '', VERCEL_CLI_VERSION: '47.0.0' }), { version: '47.0.0' })
	assert.deepStrictEqual(resolveCliVersion({ INPUT_VERCEL_CLI_VERSION: '  49.1.2 \n' }), { version: '49.1.2' })
	assert.deepStrictEqual(resolveCliVersion({ INPUT_VERCEL_CLI_VERSION: 'false' }), { disabled: true })
})

test('rejects non-exact versions', () => {
	const error = { error: 'VERCEL_CLI_VERSION must be an exact version like 48.0.0, or false' }
	for (const value of [ 'latest', '^48.0.0', '48', '48.0', 'v48.0.0', 'False', '   ' ]) {
		assert.deepStrictEqual(resolveCliVersion({ INPUT_VERCEL_CLI_VERSION: value }), error, JSON.stringify(value))
	}
})

test('puts the CLI folder first on PATH', () => {
	assert.deepStrictEqual(prependToPath({ PATH: '/usr/bin', A: '1' }, '/cli/bin'), { PATH: `/cli/bin${ path.delimiter }/usr/bin`, A: '1' })
	assert.deepStrictEqual(prependToPath({}, '/cli/bin'), { PATH: '/cli/bin' })
})

test('installs the default CLI and hands its folder to the binary', async () => {
	const { code, spawned, ensured, lines } = await runMain()
	assert.strictEqual(code, 7)
	assert.deepStrictEqual(ensured, [ '48.0.0' ])
	assert.strictEqual(spawned[0].PATH, `/cli/bin${ path.delimiter }/usr/bin`)
	assert.deepStrictEqual(lines, [])
})

test('VERCEL_CLI_VERSION false keeps v2.0.0 behaviour', async () => {
	let loaded = false
	const { code, spawned } = await runMain({
		env: { PATH: '/usr/bin', INPUT_VERCEL_CLI_VERSION: 'false' },
		loadCliInstall: () => { loaded = true; return null }
	})
	assert.strictEqual(code, 7)
	assert.strictEqual(loaded, false)
	assert.strictEqual(spawned[0].PATH, '/usr/bin')
})

test('an invalid version fails before anything runs', async () => {
	let loaded = false
	const { code, spawned, lines } = await runMain({
		env: { INPUT_VERCEL_CLI_VERSION: 'latest' },
		loadCliInstall: () => { loaded = true; return null }
	})
	assert.strictEqual(code, 1)
	assert.strictEqual(loaded, false)
	assert.deepStrictEqual(spawned, [])
	assert.deepStrictEqual(lines, [ '::error::VERCEL_CLI_VERSION must be an exact version like 48.0.0, or false\n' ])
})

test('a missing bundle is a clear error (real loader)', async () => {
	const { code, spawned, lines } = await runMain({ loadCliInstall: undefined })
	assert.strictEqual(code, 1)
	assert.deepStrictEqual(spawned, [])
	assert.deepStrictEqual(lines, [ '::error::Action bundle not found; reference a release tag such as @v2\n' ])
})

test('install errors become one escaped ::error:: annotation', async () => {
	const { code, spawned, lines } = await runMain({
		loadCliInstall: () => ({ ensureVercelCli: async () => { throw new Error('npm install vercel@48.0.0 failed:\nnpm ERR! 404 Not Found') } })
	})
	assert.strictEqual(code, 1)
	assert.deepStrictEqual(spawned, [])
	assert.deepStrictEqual(lines, [ '::error::npm install vercel@48.0.0 failed:%0Anpm ERR! 404 Not Found\n' ])
})

test('platform errors still come first', async () => {
	const { code, lines } = await runMain({ platform: 'darwin', arch: 'arm64', env: { INPUT_VERCEL_CLI_VERSION: 'latest' } })
	assert.strictEqual(code, 1)
	assert.deepStrictEqual(lines, [ '::error::Unsupported runner platform darwin-arm64; supported: linux-x64, linux-arm64\n' ])
})
