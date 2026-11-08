#!/usr/bin/env node
// Checks that the LuCI application's menu, ACL and view files agree, so a page
// cannot ship with a menu entry that renders nothing or a helper it may not run.
//
// The views are plain scripts the LuCI loader wraps in a function, so this file
// is the only JS here that runs as a module; the views are parsed, not imported.
import { readFileSync, existsSync } from 'node:fs';

const base = 'deploy/openwrt/luci-app-iptv';
const read = (path) => JSON.parse(readFileSync(`${base}/${path}`, 'utf8'));

const menu = read('root/usr/share/luci/menu.d/luci-app-iptv.json');
const acl = read('root/usr/share/rpcd/acl.d/luci-app-iptv.json');
const errors = [];

// Every route entry points at a view file, and the top-level entries use
// "firstchild" instead.
for (const [route, entry] of Object.entries(menu)) {
	const action = entry.action ?? {};
	if (action.type === 'view') {
		if (!action.path) errors.push(`${route}: view entry without a path`);
		else if (!existsSync(`${base}/htdocs/luci-static/resources/view/${action.path}.js`))
			errors.push(`${route}: view file htdocs/luci-static/resources/view/${action.path}.js is missing`);
	} else if (action.type !== 'firstchild') {
		errors.push(`${route}: unknown action type ${action.type}`);
	}
}

// The subtree root must point at a child that exists.
for (const [route, entry] of Object.entries(menu)) {
	if (entry.action?.type !== 'firstchild') continue;
	const children = Object.keys(menu).filter((other) => other.startsWith(`${route}/`));
	if (children.length === 0) errors.push(`${route}: firstchild has no children`);
}

// The ACL must grant what the views actually use: uci read/write on `iptv`, and
// exec on every helper the views call through fs.exec.
const grant = acl['luci-app-iptv'];
if (!grant) errors.push('acl: no luci-app-iptv section');
else {
	if (!grant.read?.uci?.includes('iptv')) errors.push('acl: read.uci must include iptv');
	if (!grant.write?.uci?.includes('iptv')) errors.push('acl: write.uci must include iptv');
	// A view may run a read helper (status, log) or a write one (restart), so
	// both grants count.
	const scripts = [
		...Object.keys(grant.read?.file ?? {}),
		...Object.keys(grant.write?.file ?? {})
	];
	for (const entry of Object.values(menu)) {
		const path = entry.action?.path;
		if (!path) continue;
		const view = readFileSync(`${base}/htdocs/luci-static/resources/view/${path}.js`, 'utf8');
		for (const match of view.matchAll(/fs\.exec\(\s*'([^']+)'/g)) {
			const script = match[1];
			if (!scripts.includes(script))
				errors.push(`acl: ${path}.js runs ${script}, which neither read.file nor write.file grants`);
		}
	}
}

if (errors.length > 0) {
	for (const error of errors) console.error(error);
	process.exit(1);
}
console.log(`menu, acl and views agree (${Object.keys(menu).length} routes)`);
