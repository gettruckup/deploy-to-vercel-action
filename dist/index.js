'use strict'
// Launcher for deploy-to-vercel-action v2: the action logic is a static Rust binary in dist/bin/.
const { spawnSync } = require('child_process')
const fs = require('fs')
const path = require('path')

const TARGETS = {
	'linux-x64': 'x86_64-unknown-linux-musl',
	'linux-arm64': 'aarch64-unknown-linux-musl'
}

const resolveBinary = (platform, arch, dir) => {
	const key = `${ platform }-${ arch }`
	const target = TARGETS[key]
	if (!target) return { error: `Unsupported runner platform ${ key }; supported: ${ Object.keys(TARGETS).join(', ') }` }

	const bin = path.join(dir, 'bin', `deploy-to-vercel-${ target }`)
	if (!fs.existsSync(bin)) return { error: 'Action binary not found; reference a release tag such as @v2' }

	return { bin }
}

const main = () => {
	const { bin, error } = resolveBinary(process.platform, process.arch, __dirname)
	if (error) {
		process.stdout.write(`::error::${ error }\n`)
		return 1
	}

	try {
		fs.chmodSync(bin, 0o755)
	} catch (err) {
		// best effort: release commits already carry the executable bit
	}

	const result = spawnSync(bin, [], { stdio: 'inherit', env: process.env })
	if (result.error) {
		process.stdout.write(`::error::Failed to start action binary: ${ result.error.message }\n`)
		return 1
	}

	return result.status === null ? 1 : result.status
}

if (require.main === module) process.exit(main())

module.exports = { resolveBinary, TARGETS }
