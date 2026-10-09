// Installs the Vercel CLI for the launcher and caches it across runs with GitHub's cache service.
// Bundled to dist/cli-install.cjs at release time; dist/index.js loads it only when VERCEL_CLI_VERSION is in effect.
import * as actionsCache from '@actions/cache'
import { spawnSync as nodeSpawnSync } from 'node:child_process'
import { existsSync as nodeExistsSync, rmSync as nodeRmSync } from 'node:fs'
import os from 'node:os'
import path from 'node:path'

const escapeData = (value) => String(value).replace(/%/g, '%25').replace(/\r/g, '%0D').replace(/\n/g, '%0A')

const stdoutLog = {
	info: (message) => process.stdout.write(`${ message }\n`),
	warning: (message) => process.stdout.write(`::warning::${ escapeData(message) }\n`),
	group: (title) => process.stdout.write(`::group::${ escapeData(title) }\n`),
	endGroup: () => process.stdout.write('::endgroup::\n')
}

export const defaultDeps = {
	cache: actionsCache,
	spawnSync: nodeSpawnSync,
	existsSync: nodeExistsSync,
	rmSync: nodeRmSync,
	log: stdoutLog,
	env: process.env,
	now: () => Date.now()
}

export const installFolder = (version, env) =>
	path.join(env.RUNNER_TOOL_CACHE || os.tmpdir(), 'deploy-to-vercel-action', 'vercel-cli', version)

export const cacheKey = (version, env, nodeMajor) =>
	`deploy-to-vercel-action-vercel-cli-${ version }-${ env.ImageOS || process.platform }-${ process.arch }-node${ nodeMajor }`

// The CLI runs on the `node` found on PATH, so its major version is part of the cache key.
const nodeMajorOnPath = (spawnSync) => {
	const result = spawnSync('node', [ '--version' ], { encoding: 'utf8' })
	const match = /^v(\d+)\./.exec((result && result.stdout) || '')
	return match ? match[1] : 'unknown'
}

const elapsed = (startedAt, now) => ((now() - startedAt) / 1000).toFixed(1)

// A stalled cache download must not hold a deploy for the library's 10-minute default; npm takes ~20s.
const RESTORE_SEGMENT_TIMEOUT_MS = 120000

// npm and its dependencies' install scripts never need the action's credentials.
const CREDENTIAL_ENV = /^(INPUT_|ACTIONS_)|^(GITHUB_TOKEN|GH_TOKEN|GH_PAT|VERCEL_TOKEN)$/

export const npmEnv = (env) => Object.fromEntries(Object.entries(env).filter(([ key ]) => !CREDENTIAL_ENV.test(key)))

export const ensureVercelCli = async (version, deps = defaultDeps) => {
	const { cache, spawnSync, existsSync, rmSync, log, env, now } = deps
	const folder = installFolder(version, env)
	const binDir = path.join(folder, 'bin')
	const cli = path.join(binDir, 'vercel')
	log.group(`Vercel CLI ${ version }`)
	try {
		if (existsSync(cli)) {
			log.info(`Using Vercel CLI ${ version } already installed at ${ folder }`)
			return binDir
		}

		const key = cacheKey(version, env, nodeMajorOnPath(spawnSync))
		const cacheAvailable = cache.isFeatureAvailable()
		if (cacheAvailable) {
			const startedAt = now()
			try {
				const hit = await cache.restoreCache([ folder ], key, undefined, { segmentTimeoutInMs: RESTORE_SEGMENT_TIMEOUT_MS })
				if (hit && existsSync(cli)) {
					log.info(`Restored Vercel CLI ${ version } from cache in ${ elapsed(startedAt, now) }s`)
					return binDir
				}
			} catch (err) {
				log.warning(`Could not restore Vercel CLI from cache: ${ err.message }`)
			}
		}

		// A failed restore or install can leave a partial tree whose bin/vercel the next run would reuse.
		const clearFolder = () => rmSync(folder, { recursive: true, force: true })
		clearFolder()
		const startedAt = now()
		try {
			const args = [ 'install', '--global', '--prefix', folder, `vercel@${ version }`, '--no-audit', '--no-fund', '--loglevel=error' ]
			const result = spawnSync('npm', args, { encoding: 'utf8', env: npmEnv(env) })
			if (result.error) {
				if (result.error.code === 'ENOENT') throw new Error('Vercel CLI install needs npm on PATH (or set VERCEL_CLI_VERSION: false)')
				throw new Error(`Could not run npm: ${ result.error.message }`)
			}
			if (result.status !== 0) throw new Error(`npm install vercel@${ version } failed:\n${ (result.stderr || '').trim() }`)
			if (!existsSync(cli)) throw new Error(`npm install did not produce ${ cli }`)
		} catch (err) {
			clearFolder()
			throw err
		}
		log.info(`Installed Vercel CLI ${ version } with npm in ${ elapsed(startedAt, now) }s`)

		if (cacheAvailable) {
			try {
				const cacheId = await cache.saveCache([ folder ], key)
				if (typeof cacheId === 'number' && cacheId < 0) {
					log.info(`Vercel CLI ${ version } was not saved to cache`)
				} else {
					log.info(`Saved Vercel CLI ${ version } to cache`)
				}
			} catch (err) {
				log.warning(`Could not save Vercel CLI to cache: ${ err.message }`)
			}
		}
		return binDir
	} finally {
		log.endGroup()
	}
}
