// Exercise the real packager in a disposable source tree. No release binaries
// are built or installed; /usr/bin/true stands in for each executable.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {spawnSync} = require('node:child_process');
const test = require('node:test');

test('release bundle includes runtime assets and excludes preview tools', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'localwispr-bundle-test-'));
    const uuid = 'localwispr@localwispr.github.io';
    const extension = path.join(root, 'gnome-extension', uuid);
    try {
        for (const directory of ['packaging', 'target/release', `gnome-extension/${uuid}`, 'gnome-extension/tests'])
            fs.mkdirSync(path.join(root, directory), {recursive: true});
        fs.copyFileSync(path.join(__dirname, '../bundle.sh'), path.join(root, 'packaging/bundle.sh'));
        fs.writeFileSync(path.join(root, 'Cargo.toml'), 'version = "9.9.9"\n');
        fs.writeFileSync(path.join(root, 'LICENSE'), 'test license\n');
        fs.writeFileSync(path.join(root, 'setup-permissions.sh'), '#!/bin/sh\nexit 0\n');
        for (const binary of ['localwispr', 'localwispr-llm'])
            fs.copyFileSync('/usr/bin/true', path.join(root, 'target/release', binary));
        const runtime = ['extension.js', 'metadata.json', 'stylesheet.css'];
        for (const filename of runtime)
            fs.writeFileSync(path.join(extension, filename), `runtime: ${filename}\n`);
        // Even an accidentally misplaced development file must stay out.
        for (const filename of ['dev-only.js', 'dev-tool.py', 'sample.webm'])
            fs.writeFileSync(path.join(extension, filename), 'development only\n');
        fs.writeFileSync(path.join(root, 'gnome-extension/tests/development-test.py'), 'test only\n');
        fs.writeFileSync(path.join(root, 'gnome-extension/build-clipboard.sh'),
            '#!/bin/sh\n' +
            'touch "$1/liblocalwispr-selection.so" "$1/LocalwisprSelection-1.0.typelib"\n' +
            'printf "1\\n" > "$1/selection-abi"\n', {mode: 0o755});
        const result = spawnSync('bash', [path.join(root, 'packaging/bundle.sh'),
            '--variant', 'cpu', '--channel', 'release', '--archive'],
        {encoding: 'utf8', env: {...process.env, XZ_OPT: '-T1 -0'}});
        assert.equal(result.status, 0, result.stderr);
        const bundle = result.stdout.trim();
        const installed = path.join(bundle, 'share/gnome-shell/extensions', uuid);
        assert.deepEqual(fs.readdirSync(installed).sort(), [...runtime,
            'liblocalwispr-selection.so', 'LocalwisprSelection-1.0.typelib', 'selection-abi'].sort());
        for (const filename of runtime)
            assert.equal(fs.readFileSync(path.join(installed, filename), 'utf8'), `runtime: ${filename}\n`);
        const archive = spawnSync('tar', ['-tJf', `${bundle}.tar.xz`], {encoding: 'utf8'});
        assert.equal(archive.status, 0, archive.stderr);
        assert.ok(!/dev-only|dev-tool|sample\.webm|\/tests\//.test(archive.stdout));
    } finally {
        fs.rmSync(root, {recursive: true, force: true});
    }
});
