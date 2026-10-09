'use strict'
const test = require('node:test')
const assert = require('node:assert')
const fs = require('fs')
const os = require('os')
const path = require('path')
const { spawnSync } = require('child_process')

const SHIM = path.join(__dirname, '..', '..', 'dist', 'index.js')
const { resolveBinary } = require(SHIM)

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
	const result = spawnSync(process.execPath, [ path.join(dist, 'index.js') ], { env: { ...process.env, INPUT_FOO: 'bar' }, encoding: 'utf8' })
	assert.strictEqual(result.status, 7)
	assert.strictEqual(result.stdout, 'input=bar\n')
})
