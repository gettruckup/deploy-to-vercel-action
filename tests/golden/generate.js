'use strict'
// Golden vectors from the v1 (Node) implementation at commit a8d4a64.
// Setup: npm install --prefix tests/golden --no-save action-input-parser@1.2.38
// Run:   node tests/golden/generate.js
// Code marked "v1:" is copied unchanged from the a8d4a64 source named next to it.

const fs = require('fs')
const path = require('path')
const crypto = require('crypto')

process.chdir(__dirname) // action-input-parser loads .env from the cwd; this directory has none
const parser = require('action-input-parser')

const write = (name, data) => fs.writeFileSync(path.join(__dirname, name), `${ JSON.stringify(data, null, 2) }\n`)
const orNull = (value) => (value === undefined ? null : value)
const omit = (object, key) => {
	const copy = { ...object }
	delete copy[key]
	return copy
}

// ---------------------------------------------------------------- inputs
const IS_PR = false
// v1: src/config.js `context` (inputs only; RUNNING_LOCAL is not an input)
const readConfig = () => ({
	GITHUB_TOKEN: parser.getInput({ key: [ 'GH_PAT', 'GITHUB_TOKEN' ], required: true }),
	VERCEL_TOKEN: parser.getInput({ key: 'VERCEL_TOKEN', required: true }),
	VERCEL_ORG_ID: parser.getInput({ key: 'VERCEL_ORG_ID', required: true }),
	VERCEL_PROJECT_ID: parser.getInput({ key: 'VERCEL_PROJECT_ID', required: true }),
	PRODUCTION: parser.getInput({ key: 'PRODUCTION', type: 'boolean', default: !IS_PR }),
	GITHUB_DEPLOYMENT: parser.getInput({ key: 'GITHUB_DEPLOYMENT', type: 'boolean', default: true }),
	CREATE_COMMENT: parser.getInput({ key: 'CREATE_COMMENT', type: 'boolean', default: true }),
	DELETE_EXISTING_COMMENT: parser.getInput({ key: 'DELETE_EXISTING_COMMENT', type: 'boolean', default: true }),
	ATTACH_COMMIT_METADATA: parser.getInput({ key: 'ATTACH_COMMIT_METADATA', type: 'boolean', default: true }),
	DEPLOY_PR_FROM_FORK: parser.getInput({ key: 'DEPLOY_PR_FROM_FORK', type: 'boolean', default: false }),
	PR_LABELS: parser.getInput({ key: 'PR_LABELS', default: [ 'deployed' ], type: 'array', disableable: true }),
	ALIAS_DOMAINS: parser.getInput({ key: 'ALIAS_DOMAINS', type: 'array', disableable: true }),
	PR_PREVIEW_DOMAIN: parser.getInput({ key: 'PR_PREVIEW_DOMAIN' }),
	VERCEL_SCOPE: parser.getInput({ key: 'VERCEL_SCOPE' }),
	GITHUB_REPOSITORY: parser.getInput({ key: 'GITHUB_REPOSITORY', required: true }),
	GITHUB_DEPLOYMENT_ENV: parser.getInput({ key: 'GITHUB_DEPLOYMENT_ENV' }),
	TRIM_COMMIT_MESSAGE: parser.getInput({ key: 'TRIM_COMMIT_MESSAGE', type: 'boolean', default: false }),
	WORKING_DIRECTORY: parser.getInput({ key: 'WORKING_DIRECTORY' }),
	BUILD_ENV: parser.getInput({ key: 'BUILD_ENV', type: 'array' }),
	PREBUILT: parser.getInput({ key: 'PREBUILT', type: 'boolean', default: false }),
	FORCE: parser.getInput({ key: 'FORCE', type: 'boolean', default: false })
})

const INPUT_KEYS = [
	'GH_PAT', 'GITHUB_TOKEN', 'VERCEL_TOKEN', 'VERCEL_ORG_ID', 'VERCEL_PROJECT_ID', 'PRODUCTION', 'GITHUB_DEPLOYMENT',
	'CREATE_COMMENT', 'DELETE_EXISTING_COMMENT', 'ATTACH_COMMIT_METADATA', 'DEPLOY_PR_FROM_FORK', 'PR_LABELS', 'ALIAS_DOMAINS',
	'PR_PREVIEW_DOMAIN', 'VERCEL_SCOPE', 'GITHUB_REPOSITORY', 'GITHUB_DEPLOYMENT_ENV', 'TRIM_COMMIT_MESSAGE', 'WORKING_DIRECTORY',
	'BUILD_ENV', 'PREBUILT', 'FORCE'
]
const resetEnv = () => {
	for (const key of Object.keys(process.env)) {
		if (key.startsWith('INPUT_') || INPUT_KEYS.includes(key)) delete process.env[key]
	}
}

const BASE = {
	INPUT_GITHUB_TOKEN: 'gh-token',
	INPUT_VERCEL_TOKEN: 'vercel-token',
	INPUT_VERCEL_ORG_ID: 'team_org',
	INPUT_VERCEL_PROJECT_ID: 'prj_1',
	GITHUB_REPOSITORY: 'octo/repo'
}

const INPUT_CASES = [
	{ name: 'defaults', env: BASE },
	{ name: 'input wins over plain env', env: { ...BASE, VERCEL_SCOPE: 'plain', INPUT_VERCEL_SCOPE: 'input' } },
	{ name: 'plain env fallback', env: { ...BASE, VERCEL_SCOPE: 'plain' } },
	{ name: 'empty input falls back to plain env', env: { ...BASE, INPUT_VERCEL_SCOPE: '', VERCEL_SCOPE: 'plain' } },
	{ name: 'strings are trimmed', env: { ...BASE, INPUT_PR_PREVIEW_DOMAIN: '  pr{PR}.example.com \n' } },
	{ name: 'whitespace-only string stays empty', env: { ...BASE, INPUT_WORKING_DIRECTORY: '   ' } },
	{ name: 'GH_PAT wins over GITHUB_TOKEN', env: { ...BASE, GH_PAT: 'pat' } },
	{ name: 'GITHUB_TOKEN from plain env', env: { ...omit(BASE, 'INPUT_GITHUB_TOKEN'), GITHUB_TOKEN: 'plain-token' } },
	{ name: 'missing GitHub token', env: omit(BASE, 'INPUT_GITHUB_TOKEN') },
	{ name: 'missing Vercel token', env: omit(BASE, 'INPUT_VERCEL_TOKEN') },
	{ name: 'missing repository', env: omit(BASE, 'GITHUB_REPOSITORY') },
	...[ 'true', 'True', 'TRUE', 'false', 'False', 'FALSE' ].map((value) => ({ name: `boolean ${ value }`, env: { ...BASE, INPUT_FORCE: value } })),
	{ name: 'boolean rejects yes', env: { ...BASE, INPUT_PRODUCTION: 'yes' } },
	{ name: 'boolean is not trimmed', env: { ...BASE, INPUT_PREBUILT: 'true ' } },
	{ name: 'boolean from plain env', env: { ...BASE, PRODUCTION: 'false' } },
	{ name: 'array newline separated', env: { ...BASE, INPUT_ALIAS_DOMAINS: 'a.example.com\nb.example.com\n' } },
	{ name: 'array comma separated', env: { ...BASE, INPUT_ALIAS_DOMAINS: 'a.example.com, b.example.com' } },
	{ name: 'array mixed separators', env: { ...BASE, INPUT_BUILD_ENV: 'A=1,B=2\nC=3' } },
	{ name: 'array splits values on commas', env: { ...BASE, INPUT_BUILD_ENV: 'LIST=a,b' } },
	{ name: 'array whitespace-only entry', env: { ...BASE, INPUT_ALIAS_DOMAINS: 'a.example.com\n  \nb.example.com' } },
	{ name: 'array of only a newline', env: { ...BASE, INPUT_ALIAS_DOMAINS: '\n' } },
	{ name: 'labels disabled', env: { ...BASE, INPUT_PR_LABELS: 'false' } },
	{ name: 'labels disabled via plain env', env: { ...BASE, PR_LABELS: 'false' } },
	{ name: 'labels False is a label', env: { ...BASE, INPUT_PR_LABELS: 'False' } },
	{ name: 'custom labels', env: { ...BASE, INPUT_PR_LABELS: 'deployed,preview' } },
	{ name: 'alias domains disabled', env: { ...BASE, INPUT_ALIAS_DOMAINS: 'false' } },
	{
		name: 'consumer preview workflow',
		env: {
			...BASE,
			INPUT_PRODUCTION: 'false',
			INPUT_PR_LABELS: 'false',
			INPUT_VERCEL_SCOPE: 'truckup-591e6e55',
			INPUT_ALIAS_DOMAINS: '\n',
			INPUT_GITHUB_DEPLOYMENT_ENV: 'pr12',
			INPUT_BUILD_ENV: 'NEXT_PUBLIC_STAGE_NAME=pr12\nNEXT_PUBLIC_API_URL=https://pr12.api.example.com\n'
		}
	}
]

write('inputs.json', INPUT_CASES.map(({ name, env }) => {
	resetEnv()
	Object.assign(process.env, env)
	try {
		const ok = {}
		for (const [ key, value ] of Object.entries(readConfig())) ok[key] = orNull(value)
		return { name, env, ok }
	} catch (err) {
		return { name, env, err: err.message }
	}
}))

// ---------------------------------------------------------------- aliases
// v1: src/index.js
const urlSafeParameter = (input) => input.replace(/[^a-z0-9_~]/gi, '-')
// v1: src/helpers.js
const addSchema = (url) => {
	const regex = /^https?:\/\//
	if (!regex.test(url)) {
		return `https://${ url }`
	}

	return url
}
// v1: src/helpers.js
const removeSchema = (url) => {
	const regex = /^https?:\/\//
	return url.replace(regex, '')
}

// v1: src/index.js run(), PR_PREVIEW_DOMAIN branch (core.warning/core.info replaced by returning truncatedFrom)
const previewAlias = ({ template, USER, REPOSITORY, BRANCH, PR_NUMBER, SHA }) => {
	const alias = template.replace('{USER}', urlSafeParameter(USER))
		.replace('{REPO}', urlSafeParameter(REPOSITORY))
		.replace('{BRANCH}', urlSafeParameter(BRANCH))
		.replace('{PR}', PR_NUMBER)
		.replace('{SHA}', SHA.substring(0, 7))
		.toLowerCase()

	const previewDomainSuffix = '.vercel.app'
	let nextAlias = alias
	let truncatedFrom = null

	if (alias.endsWith(previewDomainSuffix)) {
		let prefix = alias.substring(0, alias.indexOf(previewDomainSuffix))

		if (prefix.length >= 60) {
			truncatedFrom = prefix
			prefix = prefix.substring(0, 55)
			const uniqueSuffix = crypto.createHash('sha256')
				.update(`git-${ BRANCH }-${ REPOSITORY }`)
				.digest('hex')
				.slice(0, 6)

			nextAlias = `${ prefix }-${ uniqueSuffix }${ previewDomainSuffix }`
		}
	}

	return { alias: nextAlias, truncatedFrom }
}

// v1: src/index.js run(), ALIAS_DOMAINS loop body
const domainAlias = ({ template, USER, REPOSITORY, BRANCH, SHA }) => template
	.replace('{USER}', urlSafeParameter(USER))
	.replace('{REPO}', urlSafeParameter(REPOSITORY))
	.replace('{BRANCH}', urlSafeParameter(BRANCH))
	.replace('{SHA}', SHA.substring(0, 7))
	.toLowerCase()

const VARS = { USER: 'octo', REPOSITORY: 'repo', BRANCH: 'feature/login-form', PR_NUMBER: '7', SHA: 'abcdef0123456789' }
const PREVIEW_CASES = [
	{ ...VARS, template: 'pr{PR}.app.example.com' },
	{ ...VARS, template: '{REPO}-{BRANCH}.vercel.app' },
	{ ...VARS, template: '{USER}-{REPO}-{SHA}.vercel.app' },
	{ ...VARS, template: 'PR-{PR}.Example.COM' },
	{ ...VARS, USER: 'Octo_Org', REPOSITORY: 'My.Repo', BRANCH: 'Feature/ÑANDÚ 🚀', template: '{USER}-{REPO}-{BRANCH}.vercel.app' },
	{ ...VARS, BRANCH: `feature/${ 'a'.repeat(51) }`, template: '{BRANCH}.vercel.app' },
	{ ...VARS, BRANCH: `feature/${ 'a'.repeat(52) }`, template: '{BRANCH}.vercel.app' },
	{ ...VARS, BRANCH: 'feature/this-is-a-very-long-branch-name-that-keeps-going-and-going', template: '{REPO}-{BRANCH}.vercel.app' },
	{ ...VARS, BRANCH: 'x'.repeat(80), template: '{BRANCH}.example.com' },
	{ ...VARS, template: '{BRANCH}.vercel.app.example.com' },
	{ ...VARS, BRANCH: 'y'.repeat(70), template: 'https://{BRANCH}.vercel.app' }
]
const DOMAIN_CASES = [
	{ ...VARS, BRANCH: 'main', template: '{BRANCH}.example.com' },
	{ ...VARS, template: 'App.Example.com' },
	{ ...VARS, template: '{PR}.example.com' },
	{ ...VARS, template: '{USER}-{REPO}-{SHA}.example.com' },
	{ ...VARS, BRANCH: '1.2.3', template: '{BRANCH}.example.com' }
]

write('aliases.json', {
	urlSafe: [ 'main', 'feature/login-form', 'Feature_Branch~1', 'dots.and spaces', 'ñandú', '🚀', '' ]
		.map((input) => ({ input, output: urlSafeParameter(input) })),
	schema: [ 'example.com', 'https://example.com', 'http://example.com', 'HTTPS://example.com', 'ftp://example.com' ]
		.map((input) => ({ input, add: addSchema(input), remove: removeSchema(input) })),
	preview: PREVIEW_CASES.map((c) => ({ ...c, ...previewAlias(c) })),
	domain: DOMAIN_CASES.map((c) => ({ ...c, alias: domainAlias(c) }))
})

// ---------------------------------------------------------------- comments
// v1: src/github.js createComment()
const dedent = (body) => body.replace(/^[^\S\n]+/gm, '')
// v1: src/index.js run(), fork refusal body
const forkBody = (ACTOR, USER) => `
			Refusing to deploy this Pull Request to Vercel because it originates from @${ ACTOR }'s fork.

			**@${ USER }** To allow this behaviour set \`DEPLOY_PR_FROM_FORK\` to true ([more info](https://github.com/BetaHuhn/deploy-to-vercel-action#deploying-a-pr-made-from-a-fork-or-dependabot)).
		`
// v1: src/index.js run(), deployed body
const deployedBody = (SHA, previewUrl, inspectorUrl, LOG_URL) => `
					This pull request has been deployed to Vercel.

					<table>
						<tr>
							<td><strong>Latest commit:</strong></td>
							<td><code>${ SHA.substring(0, 7) }</code></td>
						</tr>
						<tr>
							<td><strong>✅ Preview:</strong></td>
							<td><a href='${ previewUrl }'>${ previewUrl }</a></td>
						</tr>
						<tr>
							<td><strong>🔍 Inspect:</strong></td>
							<td><a href='${ inspectorUrl }'>${ inspectorUrl }</a></td>
						</tr>
					</table>

					[View Workflow Logs](${ LOG_URL })
				`

const DEPLOYED_CASES = [
	{ sha: 'abcdef0123456789', previewUrl: 'https://pr7.app.example.com', inspectorUrl: 'https://vercel.com/octo/repo/dpl1', logUrl: 'https://github.com/octo/repo/actions/runs/99' },
	{ sha: 'abc', previewUrl: 'https://x.vercel.app', inspectorUrl: '', logUrl: 'https://github.com/octo/repo' }
]
write('comments.json', {
	fork: [ { actor: 'contributor', user: 'octo', body: dedent(forkBody('contributor', 'octo')) } ],
	deployed: DEPLOYED_CASES.map((c) => ({ ...c, body: dedent(deployedBody(c.sha, c.previewUrl, c.inspectorUrl, c.logUrl)) }))
})

// ---------------------------------------------------------------- CLI stdout
// v1: src/helpers.js exec() resolves stdout.trim(); src/vercel.js deploy() parses it
const parseDeploymentUrl = (stdout) => {
	const output = stdout.trim()
	const match = output.match(/(?<=https?:\/\/)(.*)/g)
	if (match === null) return { error: 'TypeError: match returned null' }
	const parsed = match[0]
	if (!parsed) return { error: 'Could not parse deploymentUrl' }
	return { host: parsed }
}

const CLI_STDOUT = [
	'https://proj-abc123.vercel.app\n',
	'https://proj-abc123-truckup.vercel.app',
	'Vercel CLI 22.0.1\nhttps://proj-abc123.vercel.app\n',
	'🔍  Inspect: https://vercel.com/truckup/proj/9xYz [1s]\n✅  Production: https://proj-abc123.vercel.app [12s]\n',
	'http://localhost:3000\n',
	'https://proj.vercel.app\r\nmore',
	'xhttps://a.vercel.app http://b.vercel.app',
	'',
	'no url here',
	'https://',
	'  \n https://spaced.vercel.app  \n'
]
write('cli-stdout.json', CLI_STDOUT.map((stdout) => ({ stdout, ...parseDeploymentUrl(stdout) })))
