import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {digest} from './fixtures.mjs';

// Standalone spec: the owner selects this spec with a fresh disposable fixture.
// It never imports a saved session, edits source bytes, or simulates a vote.
const root = process.env.IMAGE_RANKER_E2E_ROOT;
assert.ok(root, 'The desktop fixture root must be supplied by the test runner');
assert.equal(process.platform, 'win32', 'This qualification targets Windows WebView2');
assert.equal(fs.readFileSync(path.join(root, '.image-ranker-e2e'), 'utf8'), 'image-ranker-disposable-fixture-v1\n');
const inventory = JSON.parse(fs.readFileSync(path.join(root, 'inventory.json'), 'utf8'));
const originals = [...inventory, ...['corrupt.bmp', 'notes.txt'].map(name => {
  const file = path.join(root, 'source', name);
  return {path: file, hash: digest(file)};
})];
const evidence = [];
const keys = {shift: '\uE008', alt: '\uE00A', up: '\uE013', down: '\uE015', end: '\uE010', enter: '\uE007', release: '\uE000'};
let images;
let keyboardTarget;

async function native(command, args = {}) {
  const {result} = await browser.executeAsync((cmd, input, done) => {
    void (async () => {
      const bootstrap = ['get_boot_status', 'preview_context', 'app_info', 'license_notices'];
      if (!bootstrap.includes(cmd) && !Object.prototype.hasOwnProperty.call(input, 'contextId'))
        input = {...input, contextId: await window.__TAURI__.core.invoke('preview_context')};
      return window.__TAURI__.core.invoke(cmd, input);
    })().then(value => done({result: {ok: true, value}}), error => done({result: {ok: false, error: String(error)}}));
  }, command, args);
  if (!result.ok) throw new Error(`${command}: ${result.error}`);
  return result.value;
}

async function saved() {
  await browser.waitUntil(async () => (await $('.save-status').getText()).includes('Saved locally'), {timeoutMsg: 'The real save queue must acknowledge this edit'});
  assert.equal((await native('session_save_status')).state, 'saved');
}

async function click(text) {
  let dialog;
  for (const candidate of await $$('[role="dialog"]')) if (await candidate.isDisplayed()) dialog = candidate;
  const target = dialog ? await dialog.$(`button=${text}`) : await $(`button=${text}`);
  await target.waitForEnabled();
  await target.click();
}

function cell(id) { return `[data-grid-id="${id}"]`; }

async function focus(selector) {
  const element = await $(selector);
  await element.waitForDisplayed();
  await element.scrollIntoView();
  const {result} = await browser.execute(value => {
    const target = document.querySelector(value);
    target.focus();
    return {result: document.activeElement === target};
  }, selector);
  assert.equal(result, true, `Keyboard focus must reach ${selector} without activating it`);
}

function sourceBytes() {
  const result = originals.map(original => ({path: original.path, hash: digest(original.path)}));
  assert.deepEqual(result, originals, 'Every original image, corrupt fixture, and unsupported file must remain byte-identical');
  return result;
}

function stateToPreserve(session) {
  return {selected: session.selected, run: session.run, review: session.review, archivedRuns: session.archivedRuns, screen: session.screen, settings: session.settings};
}

async function noNestedButtons() {
  assert.equal(await browser.execute(() => document.querySelectorAll('button button').length), 0, 'Thumbnail actions and Retry must remain separate native buttons');
}

async function pixels(selector) {
  const {result} = await browser.execute(value => {
    const image = document.querySelector(`${value} img`);
    return {result: image ? {src: image.currentSrc || image.src, complete: image.complete, width: image.naturalWidth, height: image.naturalHeight} : null};
  }, selector);
  return result;
}

async function healthyPreview(selector) {
  let result;
  await browser.waitUntil(async () => {
    result = await pixels(selector);
    return result?.complete && result.width > 0 && result.height > 0 && !result.src.startsWith('data:');
  }, {timeoutMsg: `Expected genuine native preview pixels in ${selector}`});
  return result;
}

// Only the rendered IMG source is damaged. WebView2 performs the real failed
// decode and emits an isTrusted error; no synthetic dispatch or bridge mock is
// used. Source/cache files remain untouched. The first failure may auto-retry.
async function renderedFailure(selector) {
  for (let attempt = 0; attempt < 2; attempt++) {
    if (await $(`${selector} .image-error button`).isExisting()) break;
    const before = await healthyPreview(selector);
    const {result} = await browser.execute(value => {
      const image = document.querySelector(`${value} img`);
      if (!image || !image.complete || !image.naturalWidth) throw new Error('Expected decoded native pixels before inducing the display-only failure');
      const serial = (window.__accessibilityImageErrorSerial || 0) + 1;
      window.__accessibilityImageErrorSerial = serial;
      window.__accessibilityImageErrors ||= [];
      image.addEventListener('error', event => {
        window.__accessibilityImageErrors.push({serial, trusted: event.isTrusted, failedSrc: image.src});
      }, {once: true});
      image.src = `data:image/png;base64,AA==#display-failure-${serial}`;
      return {result: {serial}};
    }, selector);
    await browser.waitUntil(() => browser.execute(serial => window.__accessibilityImageErrors.some(item => item.serial === serial), result.serial), {timeoutMsg: 'WebView2 must emit an actual failed image-load event'});
    const {result: failure} = await browser.execute(serial => ({result: window.__accessibilityImageErrors.find(item => item.serial === serial)}), result.serial);
    assert.equal(failure.trusted, true, 'A synthetic error event would not qualify native WebView display behavior');
    evidence.push({kind: 'real-display-error', selector, before, failure});
    await browser.waitUntil(async () => {
      if (await $(`${selector} .image-error button`).isExisting()) return true;
      const next = await pixels(selector);
      return next?.complete && next.width > 0 && !next.src.startsWith('data:');
    }, {timeoutMsg: 'The failed preview must show Retry or finish its single automatic retry'});
  }
  await $(`${selector} .image-error button`).waitForDisplayed();
}

async function retryWithKeyboard(selector, key, surface) {
  const before = stateToPreserve(await native('load_session'));
  const selection = await browser.execute(() => [...document.querySelectorAll('.image-action-button[aria-pressed="true"]')].map(item => item.getAttribute('aria-label')));
  await renderedFailure(selector);
  await noNestedButtons();
  const retry = `${selector} .image-error button`;
  await focus(retry);
  const {result: target} = await browser.execute(value => {
    const button = document.querySelector(value), rect = button.getBoundingClientRect();
    const hit = document.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2);
    return {result: {width: rect.width, height: rect.height, hit: hit === button || button.contains(hit), name: button.textContent}};
  }, retry);
  assert.ok(target.width >= 44 && target.height >= 44, `Retry must have an actual 44px target: ${JSON.stringify(target)}`);
  assert.equal(target.hit, true, 'The thumbnail overlay must not cover Retry');
  await browser.keys(key);
  const afterPixels = await healthyPreview(selector);
  assert.equal(await $(`${selector} .image-error button`).isExisting(), false);
  assert.equal(await $('[role="dialog"]').isExisting(), false, 'Retry must not open inspection');
  assert.deepEqual(stateToPreserve(await native('load_session')), before, 'Retry must not vote, edit ranking, or change candidate selection');
  assert.deepEqual(await browser.execute(() => [...document.querySelectorAll('.image-action-button[aria-pressed="true"]')].map(item => item.getAttribute('aria-label'))), selection, 'Local Review selection must also be preserved');
  await saved();
  evidence.push({kind: 'keyboard-retry', surface, key: key === ' ' ? 'Space' : 'Enter', target, afterPixels});
  sourceBytes();
}

async function voteLeft() {
  const before = (await native('load_session')).run.history.length;
  const vote = await $('button*=Prefer left');
  await vote.waitForEnabled();
  await vote.click();
  await browser.waitUntil(async () => (await native('load_session')).run.history.length === before + 1, {timeoutMsg: 'A real vote must persist exactly once'});
  await saved();
}

async function reviewLayout() {
  const {result} = await browser.execute(() => ({result: [...document.querySelectorAll('.review-screen .virtual-row')].map(row => [...row.querySelectorAll('[data-grid-id]')].map(item => ({id: item.dataset.gridId, top: item.getBoundingClientRect().top})))}));
  return result;
}

async function focusedAction() {
  return browser.execute(() => ({id: document.activeElement?.closest('[data-grid-id]')?.getAttribute('data-grid-id') || null, label: document.activeElement?.getAttribute('aria-label') || null, reorder: document.activeElement?.classList.contains('drag-handle') || false}));
}

async function keyboardMove(id, direction) {
  const before = await native('load_session');
  const index = before.review.order.indexOf(id), destination = index + (direction === 'up' ? -1 : 1);
  assert.ok(destination >= 0 && destination < before.review.order.length);
  const expected = [...before.review.order];
  [expected[index], expected[destination]] = [expected[destination], expected[index]];
  // Deliberately do not focus again: the preceding key must retain this action.
  await browser.keys([keys.alt, keys[direction], keys.release]);
  await browser.waitUntil(async () => JSON.stringify((await native('load_session')).review.order) === JSON.stringify(expected), {timeoutMsg: 'Alt-arrow must move the focused image by exactly one rank'});
  await browser.waitUntil(async () => {
    const focused = await focusedAction();
    return focused.id === id && focused.reorder;
  }, {timeoutMsg: 'The same image reorder action must retain focus across the rendered row change'});
  await saved();
  const after = await native('load_session');
  assert.deepEqual(after.run, before.run, 'Manual moves must retain all comparison history');
  assert.deepEqual(after.review.computed, before.review.computed);
  assert.deepEqual(after.review.picks, before.review.picks);
  assert.equal(after.review.history.length, before.review.history.length + 1);
  assert.deepEqual(after.review.history.at(-1).order, before.review.order, 'Undo must retain the exact previous order');
  evidence.push({kind: 'keyboard-move', direction, id, before: before.review.order, after: after.review.order, focus: await focusedAction(), layout: await reviewLayout()});
}

describe('Windows WebView keyboard and preview accessibility', () => {
  before(async () => {
    await browser.waitUntil(() => browser.execute(() => typeof window.__TAURI__?.core?.invoke === 'function'));
    await $('[aria-label="Help and keyboard shortcuts"]').waitForDisplayed();
    await saved();
    const current = await native('load_session');
    assert.ok(!current || (!current.report.images.length && !current.run && !current.review && !current.archivedRuns.length), 'This standalone qualification requires a fresh fixture; never reset an existing session');
    sourceBytes();
    fs.writeFileSync(path.join(root, 'source-choice.txt'), 'source', {flag: 'wx'});
    await click('Add folder');
    await browser.waitUntil(async () => (await native('load_session'))?.report.images.length === 15, {timeoutMsg: 'The real scanner must admit exactly the 15 top-level fixture images'});
    await saved();
    images = (await native('load_session')).report.images.slice().sort((a, b) => path.basename(a.path).localeCompare(path.basename(b.path)));
    assert.equal((await native('load_session')).settings.recursive, false);
    await focus('[aria-label="Thumbnail size"]');
    await browser.keys(keys.end);
    await browser.waitUntil(async () => (await native('load_session')).settings.cardSize === 260);
    await saved();
  });

  afterEach(function () {
    evidence.push({kind: 'case', title: this.currentTest.title, state: this.currentTest.state, sourceBytes: sourceBytes()});
    fs.writeFileSync(path.join(root, 'accessibility-evidence.json'), JSON.stringify({evidenceClass: 'native-webview-keyboard-and-display-error-no-narrator-or-scaling-claim', evidence}, null, 2));
  });

  it('selects a Sources range with Space and Shift+Space without opening inspection', async () => {
    await click('None'); await saved();
    await focus(`${cell(images[0].id)} .image-action-button`);
    await browser.keys(' '); await saved();
    assert.deepEqual((await native('load_session')).selected, [images[0].id]);
    await focus(`${cell(images[2].id)} .image-action-button`);
    await browser.keys([keys.shift, ' ', keys.release]); await saved();
    assert.deepEqual(new Set((await native('load_session')).selected), new Set(images.slice(0, 3).map(image => image.id)));
    assert.equal(await $('[role="dialog"]').isExisting(), false);
    assert.ok((await $('[aria-label="Selection summary"]').getText()).includes('3 selected'));
    await noNestedButtons();
    await retryWithKeyboard(`${cell(images[0].id)} .thumbnail`, ' ', 'Sources');
    await retryWithKeyboard(`${cell(images[0].id)} .thumbnail`, keys.enter, 'Sources');
  });

  it('keeps arena inspection and Retry separate from 15 genuine saved votes', async () => {
    await click('None'); await saved();
    for (const image of images.slice(0, 6)) {
      const checkbox = await $(`input[aria-label="Select ${path.basename(image.path)}"]`);
      await checkbox.scrollIntoView(); await checkbox.click(); await saved();
    }
    await $('[aria-label="Ranking mode"]').selectByAttribute('value', 'full-pairwise'); await saved();
    await click('Rank 6 images');
    const randomize = await $('//label[contains(.,"Randomize starting order")]/input');
    if (await randomize.isSelected()) { await randomize.click(); await saved(); }
    await click('Start comparing'); await saved();
    assert.equal((await native('load_session')).run.history.length, 0);
    await retryWithKeyboard('.choice:first-child .comparison-image', ' ', 'Arena');
    await retryWithKeyboard('.choice:first-child .comparison-image', keys.enter, 'Arena');
    await noNestedButtons();
    assert.equal(await $('[aria-label="Completed comparisons"]').isExisting(), true);
    for (let i = 0; i < 15; i++) await voteLeft();
    await $('.review-screen').waitForDisplayed();
    const current = await native('load_session');
    assert.equal(current.run.history.length, 15);
    assert.equal(current.review.order.length, 6);
    assert.deepEqual(new Set(current.review.order), new Set(images.slice(0, 6).map(image => image.id)));
    evidence.push({kind: 'genuine-ranking', mode: current.run.mode, decisions: current.run.history.length, computed: current.review.computed});
  });

  it('preserves local Review selection and final picks when Retry uses Space or Enter', async () => {
    const id = (await native('load_session')).review.order[0];
    const thumbnail = `${cell(id)} .thumbnail`;
    await $(`${cell(id)} .image-action-button`).click();
    assert.equal(await $(`${cell(id)} .image-action-button`).getAttribute('aria-pressed'), 'true');
    await $(`${cell(id)} .pick-button`).click(); await saved();
    assert.deepEqual((await native('load_session')).review.picks, [id]);
    await retryWithKeyboard(thumbnail, ' ', 'Review');
    await retryWithKeyboard(thumbnail, keys.enter, 'Review');
    await $(`${cell(id)} .image-action-button`).click();
    assert.equal(await $(`${cell(id)} .image-action-button`).getAttribute('aria-pressed'), 'false');
    await noNestedButtons();
  });

  it('continues Alt-arrow on the same image across Review rows in both directions', async () => {
    const rows = await reviewLayout();
    assert.ok(rows.length >= 2 && rows[0].length >= 2, `Need two real rendered rows with at least two columns: ${JSON.stringify(rows)}`);
    const target = rows[1][0].id;
    keyboardTarget = target;
    await focus(`${cell(target)} .drag-handle`);
    await keyboardMove(target, 'up');
    assert.ok((await reviewLayout())[0].some(item => item.id === target), 'The first move must actually cross the row parent');
    await keyboardMove(target, 'up');
    await keyboardMove(target, 'down');
    await keyboardMove(target, 'down');
    const after = await reviewLayout();
    assert.equal(after[1][0].id, target, 'The final downward move must cross back to the second row');
    assert.equal((await focusedAction()).id, target);
  });

  it('does not steal focus back to a reorder handle after a pointer move', async () => {
    const before = await native('load_session');
    const index = before.review.order.indexOf(keyboardTarget);
    assert.ok(index > 0);
    const expected = [...before.review.order];
    [expected[index - 1], expected[index]] = [expected[index], expected[index - 1]];
    assert.deepEqual(await focusedAction(), {id: keyboardTarget, label: `Reorder rank ${index + 1}. Use Alt plus arrow keys.`, reorder: true});
    await browser.execute(() => {
      window.__accessibilityPointerFocus = [];
      window.__accessibilityPointerStarted = false;
      window.__accessibilityPointerDown = () => { window.__accessibilityPointerStarted = true; };
      window.__accessibilityFocusIn = event => {
        if (window.__accessibilityPointerStarted) window.__accessibilityPointerFocus.push({id: event.target.closest('[data-grid-id]')?.getAttribute('data-grid-id') || null, label: event.target.getAttribute('aria-label'), reorder: event.target.classList.contains('drag-handle')});
      };
      document.addEventListener('pointerdown', window.__accessibilityPointerDown, true);
      document.addEventListener('focusin', window.__accessibilityFocusIn, true);
    });
    await $(`${cell(keyboardTarget)} button[aria-label="Move rank ${index + 1} up"]`).click();
    await browser.waitUntil(async () => JSON.stringify((await native('load_session')).review.order) === JSON.stringify(expected));
    await saved();
    const {result} = await browser.execute(() => {
      document.removeEventListener('pointerdown', window.__accessibilityPointerDown, true);
      document.removeEventListener('focusin', window.__accessibilityFocusIn, true);
      return {result: {pointerStarted: window.__accessibilityPointerStarted, focus: window.__accessibilityPointerFocus}};
    });
    assert.equal(result.pointerStarted, true, 'This must be an actual WebDriver pointer action');
    assert.ok(result.focus.some(item => item.label === `Move rank ${index + 1} up`), 'The pointer target must receive normal native-button focus');
    assert.equal(result.focus.some(item => item.reorder), false, 'No stale keyboard request may steal focus during the pointer edit');
    assert.equal((await focusedAction()).reorder, false);
    const after = await native('load_session');
    assert.deepEqual(after.run, before.run);
    assert.deepEqual(after.review.picks, before.review.picks);
    assert.equal(after.review.history.length, before.review.history.length + 1);
    evidence.push({kind: 'pointer-focus', ...result, finalFocus: await focusedAction(), before: before.review.order, after: after.review.order});
  });
});
