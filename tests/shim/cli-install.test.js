'use strict'
const test = require('node:test')
const assert = require('node:assert')
const os = require('os')
const path = require('path')

const MODULE = path.join(__dirname, '..', '..', 'launcher', 'cli-install.mjs')
const load = () => import(MODULE)

const ENV = { RUNNER_TOOL_CACHE: '/opt/hostedtoolcache', ImageOS: 'ubuntu24' }
const FOLDER = '/opt/hostedtoolcache/deploy-to-vercel-action/vercel-cli/48.0.0'
const BIN = `${ FOLDER }/bin`
const CLI = `${ BIN }/vercel`
const KEY = `deploy-to-vercel-action-vercel-cli-48.0.0-ubuntu24-${ process.arch }-node22`
const NPM = [ 'npm', 'install', '--global', '--prefix', FOLDER, 'vercel@48.0.0', '--no-audit', '--no-fund', '--loglevel=error' ]

// A fake world: which files exist, what the cache does, what node/npm do. Records calls and log lines.
const fakeDeps = (options = {}) => {
	const files = new Set(options.files || [])
	const calls = []
	const logs = []
	let clock = 0
	const deps = {
		env: options.env || ENV,
		now: () => (clock += 500),
		existsSync: (p) => files.has(p),
		log: {
			info: (m) => logs.push(`info: ${ m }`),
			warning: (m) => logs.push(`warning: ${ m }`),
			group: (m) => logs.push(`group: ${ m }`),
			endGroup: () => logs.push('endgroup')
		},
		cache: {
			isFeatureAvailable: () => options.cacheAvailable !== false,
			restoreCache: async (paths, key) => {
				calls.push([ 'restore', paths, key ])
				if (options.restoreThrows) throw new Error(options.restoreThrows)
				if (options.cacheHit) {
					if (!options.hitWithoutBinary) files.add(CLI)
					return key
				}
				return undefined
			},
			saveCache: async (paths, key) => {
				calls.push([ 'save', paths, key ])
				if (options.saveThrows) throw new Error(options.saveThrows)
				return options.saveResult === undefined ? 42 : options.saveResult
			}
		},
		spawnSync: (cmd, args) => {
			calls.push([ cmd, ...args ])
			if (cmd === 'node') {
				return options.nodeMissing
					? { error: Object.assign(new Error('spawnSync node ENOENT'), { code: 'ENOENT' }) }
					: { status: 0, stdout: 'v22.20.0\n' }
			}
			if (options.npmMissing) return { error: Object.assign(new Error('spawnSync npm ENOENT'), { code: 'ENOENT' }) }
			if (options.npmFails) return { status: 1, stdout: '', stderr: 'npm ERR! code E404\nnpm ERR! 404 Not Found\n' }
			if (!options.npmNoBinary) files.add(CLI)
			return { status: 0, stdout: '', stderr: '' }
		}
	}
	return { deps, calls, logs }
}

test('install folder and cache key follow the spec', async () => {
	const { installFolder, cacheKey } = await load()
	assert.strictEqual(installFolder('48.0.0', ENV), FOLDER)
	assert.strictEqual(installFolder('48.0.0', {}), path.join(os.tmpdir(), 'deploy-to-vercel-action', 'vercel-cli', '48.0.0'))
	assert.strictEqual(cacheKey('48.0.0', ENV, '22'), KEY)
	assert.strictEqual(cacheKey('48.0.0', {}, '22'), `deploy-to-vercel-action-vercel-cli-48.0.0-${ process.platform }-${ process.arch }-node22`)
})

test('reuses an existing install without touching cache or npm', async () => {
	const { ensureVercelCli } = await load()
	const { deps, calls, logs } = fakeDeps({ files: [ CLI ] })
	assert.strictEqual(await ensureVercelCli('48.0.0', deps), BIN)
	assert.deepStrictEqual(calls, [])
	assert.deepStrictEqual(logs, [
		'group: Vercel CLI 48.0.0',
		`info: Using Vercel CLI 48.0.0 already installed at ${ FOLDER }`,
		'endgroup'
	])
})

test('restores from cache on a hit', async () => {
	const { ensureVercelCli } = await load()
	const { deps, calls, logs } = fakeDeps({ cacheHit: true })
	assert.strictEqual(await ensureVercelCli('48.0.0', deps), BIN)
	assert.deepStrictEqual(calls, [ [ 'node', '--version' ], [ 'restore', [ FOLDER ], KEY ] ])
	assert.ok(logs.includes('info: Restored Vercel CLI 48.0.0 from cache in 0.5s'), logs.join('\n'))
	assert.strictEqual(logs.at(-1), 'endgroup')
})

test('cache miss installs with npm and saves', async () => {
	const { ensureVercelCli } = await load()
	const { deps, calls, logs } = fakeDeps()
	assert.strictEqual(await ensureVercelCli('48.0.0', deps), BIN)
	assert.deepStrictEqual(calls, [ [ 'node', '--version' ], [ 'restore', [ FOLDER ], KEY ], NPM, [ 'save', [ FOLDER ], KEY ] ])
	assert.ok(logs.includes('info: Installed Vercel CLI 48.0.0 with npm in 0.5s'), logs.join('\n'))
	assert.ok(logs.includes('info: Saved Vercel CLI 48.0.0 to cache'), logs.join('\n'))
	assert.ok(!logs.some((l) => l.startsWith('warning:')))
})

test('restore hit without the binary falls back to npm', async () => {
	const { ensureVercelCli } = await load()
	const { deps, calls } = fakeDeps({ cacheHit: true, hitWithoutBinary: true })
	assert.strictEqual(await ensureVercelCli('48.0.0', deps), BIN)
	assert.deepStrictEqual(calls.map((c) => c[0]), [ 'node', 'restore', 'npm', 'save' ])
})

test('cache unavailable installs with npm silently', async () => {
	const { ensureVercelCli } = await load()
	const { deps, calls, logs } = fakeDeps({ cacheAvailable: false })
	assert.strictEqual(await ensureVercelCli('48.0.0', deps), BIN)
	assert.deepStrictEqual(calls, [ [ 'node', '--version' ], NPM ])
	assert.ok(!logs.some((l) => l.startsWith('warning:')))
})

test('restore errors warn and fall back to npm', async () => {
	const { ensureVercelCli } = await load()
	const { deps, calls, logs } = fakeDeps({ restoreThrows: 'cache service unavailable' })
	assert.strictEqual(await ensureVercelCli('48.0.0', deps), BIN)
	assert.ok(logs.includes('warning: Could not restore Vercel CLI from cache: cache service unavailable'))
	assert.deepStrictEqual(calls.map((c) => c[0]), [ 'node', 'restore', 'npm', 'save' ])
})

test('missing npm is a clear error', async () => {
	const { ensureVercelCli } = await load()
	const { deps, logs } = fakeDeps({ cacheAvailable: false, npmMissing: true })
	await assert.rejects(ensureVercelCli('48.0.0', deps), { message: 'Vercel CLI install needs npm on PATH (or set VERCEL_CLI_VERSION: false)' })
	assert.strictEqual(logs.at(-1), 'endgroup')
})

test('failing npm reports its output', async () => {
	const { ensureVercelCli } = await load()
	const { deps } = fakeDeps({ cacheAvailable: false, npmFails: true })
	await assert.rejects(ensureVercelCli('48.0.0', deps), { message: 'npm install vercel@48.0.0 failed:\nnpm ERR! code E404\nnpm ERR! 404 Not Found' })
})

test('npm success without the binary is an error', async () => {
	const { ensureVercelCli } = await load()
	const { deps } = fakeDeps({ cacheAvailable: false, npmNoBinary: true })
	await assert.rejects(ensureVercelCli('48.0.0', deps), { message: `npm install did not produce ${ CLI }` })
})

test('save errors warn and still return the CLI', async () => {
	const { ensureVercelCli } = await load()
	const { deps, logs } = fakeDeps({ saveThrows: 'quota exceeded' })
	assert.strictEqual(await ensureVercelCli('48.0.0', deps), BIN)
	assert.ok(logs.includes('warning: Could not save Vercel CLI to cache: quota exceeded'))
	assert.ok(!logs.includes('info: Saved Vercel CLI 48.0.0 to cache'))
})

test('save returning -1 is not reported as saved', async () => {
	const { ensureVercelCli } = await load()
	const { deps, logs } = fakeDeps({ saveResult: -1 })
	assert.strictEqual(await ensureVercelCli('48.0.0', deps), BIN)
	assert.ok(!logs.includes('info: Saved Vercel CLI 48.0.0 to cache'))
	assert.ok(logs.includes('info: Vercel CLI 48.0.0 was not saved to cache'))
})

test('cache key falls back to nodeunknown', async () => {
	const { ensureVercelCli } = await load()
	const { deps, calls } = fakeDeps({ nodeMissing: true, cacheHit: true })
	await ensureVercelCli('48.0.0', deps)
	assert.deepStrictEqual(calls[1], [ 'restore', [ FOLDER ], KEY.replace('-node22', '-nodeunknown') ])
})
