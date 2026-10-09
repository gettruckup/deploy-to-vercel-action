'use strict'
// Launcher for deploy-to-vercel-action v2: the action logic is a static Rust binary in dist/bin/.
// When VERCEL_CLI_VERSION is in effect it first installs (and caches) that Vercel CLI via dist/cli-install.cjs.
const { spawnSync } = require('child_process')
const fs = require('fs')
const path = require('path')

const TARGETS = {
	'linux-x64': 'x86_64-unknown-linux-musl',
	'linux-arm64': 'aarch64-unknown-linux-musl'
}

const DEFAULT_CLI_VERSION = '48.0.0'
const CLI_VERSION_ERROR = 'VERCEL_CLI_VERSION must be an exact version like 48.0.0, or false'

const escapeData = (value) => String(value).replace(/%/g, '%25').replace(/\r/g, '%0D').replace(/\n/g, '%0A')

const resolveBinary = (platform, arch, dir) => {
	const key = `${ platform }-${ arch }`
	const target = TARGETS[key]
	if (!target) return { error: `Unsupported runner platform ${ key }; supported: ${ Object.keys(TARGETS).join(', ') }` }

	const bin = path.join(dir, 'bin', `deploy-to-vercel-${ target }`)
	if (!fs.existsSync(bin)) return { error: 'Action binary not found; reference a release tag such as @v2' }

	return { bin }
}

// Same lookup as every other input: INPUT_<KEY>, then plain env <KEY>, then the default.
const resolveCliVersion = (env) => {
	const value = (env.INPUT_VERCEL_CLI_VERSION || env.VERCEL_CLI_VERSION || DEFAULT_CLI_VERSION).trim()
	if (value === 'false') return { disabled: true }
	if (/^\d+\.\d+\.\d+$/.test(value)) return { version: value }
	return { error: CLI_VERSION_ERROR }
}

const prependToPath = (env, dir) => ({ ...env, PATH: env.PATH ? `${ dir }${ path.delimiter }${ env.PATH }` : dir })

const loadBundledCliInstall = (dir) => {
	const bundle = path.join(dir, 'cli-install.cjs')
	return fs.existsSync(bundle) ? require(bundle) : null
}

const main = async ({
	env = process.env,
	platform = process.platform,
	arch = process.arch,
	dir = __dirname,
	loadCliInstall = loadBundledCliInstall,
	spawnSync: spawn = spawnSync,
	write = (line) => process.stdout.write(line)
} = {}) => {
	const fail = (message) => {
		write(`::error::${ escapeData(message) }\n`)
		return 1
	}

	const { bin, error } = resolveBinary(platform, arch, dir)
	if (error) return fail(error)

	const cli = resolveCliVersion(env)
	if (cli.error) return fail(cli.error)

	let childEnv = env
	if (cli.version) {
		const cliInstall = loadCliInstall(dir)
		if (!cliInstall) return fail('Action bundle not found; reference a release tag such as @v2')
		try {
			childEnv = prependToPath(env, await cliInstall.ensureVercelCli(cli.version))
		} catch (err) {
			return fail(err.message)
		}
	}

	try {
		fs.chmodSync(bin, 0o755)
	} catch (err) {
		// best effort: release commits already carry the executable bit
	}

	const result = spawn(bin, [], { stdio: 'inherit', env: childEnv })
	if (result.error) return fail(`Failed to start action binary: ${ result.error.message }`)

	return result.status === null ? 1 : result.status
}

if (require.main === module) main().then((code) => process.exit(code))

module.exports = { resolveBinary, resolveCliVersion, prependToPath, main, TARGETS, DEFAULT_CLI_VERSION }
