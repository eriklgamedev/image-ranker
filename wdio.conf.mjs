import path from 'node:path';
import fs from 'node:fs';
import {createFixtures,digest} from './tests/desktop/fixtures.mjs';
import WindowsDriverService from './tests/desktop/windows-driver.mjs';
import {desktopProfile} from './tests/qualification/desktop-profile.mjs';

const qualificationProfile = desktopProfile(process.env.IMAGE_RANKER_QUALIFICATION_PROFILE, process.argv.slice(2));

const root = process.env.IMAGE_RANKER_E2E_ROOT || createFixtures();
process.env.IMAGE_RANKER_E2E_ROOT = root;
const binary = path.resolve('src-tauri/target/debug', process.platform === 'win32' ? 'image-ranker.exe' : 'image-ranker');

export const config = {
  runner: 'local',
  specs: qualificationProfile.specs,
  maxInstances: 1,
  hostname: '127.0.0.1',
  port: process.platform === 'win32' ? 4444 : 4445,
  capabilities: [{browserName: 'tauri', 'tauri:options': {application: binary}}],
  services: [[process.platform === 'win32' ? WindowsDriverService : '@wdio/tauri-service', {
    appBinaryPath: binary,
    driverProvider: process.platform === 'win32' ? 'external' : 'embedded',
    tauriDriverPath: process.platform === 'win32' ? '.cargo-home\\bin\\tauri-driver.exe' : undefined,
    nativeDriverPath: process.platform === 'win32' ? '.drivers\\msedgedriver.exe' : undefined,
    autoDownloadEdgeDriver: false,
    captureBackendLogs: true,
    captureFrontendLogs: true,
    startTimeout: 60000,
    commandTimeout: 30000,
  }]],
  framework: 'mocha',
  reporters: ['spec'],
  logLevel: 'warn',
  outputDir: path.join(root, 'driver-logs'),
  waitforTimeout: 15000,
  connectionRetryTimeout: 30000,
  connectionRetryCount: 1,
  mochaOpts: {ui: 'bdd', timeout: qualificationProfile.timeoutMs, bail: true},
  onPrepare() {
    console.log(`Disposable desktop fixture: ${root}`);
    const buildPath='tests-artifacts/e2e-build-current.json';
    const binarySha256=digest(binary);
    const captured=fs.existsSync(buildPath)?JSON.parse(fs.readFileSync(buildPath,'utf8').replace(/^\uFEFF/,'')):null;
    fs.writeFileSync(path.join(root,'build-manifest.json'),JSON.stringify({
      binarySha256,capturedBuild:captured?.binary.toLowerCase()===binarySha256?captured:null,
      qualificationProfile,
    tests:[...fs.readdirSync('tests/desktop').filter(file=>file.endsWith('.e2e.mjs')).sort().map(file=>`tests/desktop/${file}`),'tests/desktop/windows-paths.mjs','tests/desktop/windows-driver.mjs','tests/desktop/kill-app.ps1','tests/desktop/fixtures.mjs','tests/qualification/desktop-profile.mjs','wdio.conf.mjs'].map(file=>({path:file,sha256:digest(file)})),
    },null,2));
  },
  async afterTest(test, context, result) {
    fs.appendFileSync(path.join(root,'test-results.jsonl'),JSON.stringify({title:test.title,passed:result.passed,durationMs:result.duration,error:result.error?.message||null})+'\n');
    if (!result.passed) {
      const name = test.title.replace(/[^a-z0-9]+/gi, '-');
      await browser.saveScreenshot(path.join(root, `${name}.png`));
      fs.writeFileSync(path.join(root, `${name}.html`), await browser.getPageSource());
    }
  },
};
