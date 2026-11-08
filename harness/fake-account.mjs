// Create a fake account on a throwaway instance through the web UI,
// add one fake credential, log out, log in again and read it back.
// Usage: NODE_PATH=<playwright modules> node harness/fake-account.mjs [base-url]
// Prints only `ok` and counts. Passwords are random per run, never printed.
import { createRequire } from 'node:module';
import { randomBytes } from 'node:crypto';
const require = createRequire(import.meta.url);
const { chromium } = require('playwright');
const base = (process.argv[2] || 'http://127.0.0.1:18010').replace(/\/$/, '');
const mode = process.argv[3] || 'create';
const suffix = Date.now().toString(36) + randomBytes(3).toString('hex');
const rand = () => 'Aa1!' + randomBytes(18).toString('base64url');
// Optional fixed identity for drill readback on another instance:
// FAKE_USER / FAKE_ACCOUNT_PW / FAKE_CRED_PW from env, never printed.
const user = process.env.FAKE_USER || ('fake' + suffix);
const accountPw = process.env.FAKE_ACCOUNT_PW || rand();
const credPw = process.env.FAKE_CRED_PW || rand();
const service = process.env.FAKE_SERVICE || 'Fake Service';
const credUser = 'fakeuser@example.invalid';
const browser = await chromium.launch({ headless: true });
const page = await browser.newPage();
async function unlockIfNeeded() {
  if (page.url().includes('/unlock')) {
    await page.fill('#password', accountPw);
    await page.click('#unlock-button');
    await page.waitForTimeout(2500);
  }
}
async function waitAuthed() {
  for (let i = 0; i < 90; i++) {
    await unlockIfNeeded();
    if (await page.locator('#userMenuButton').isVisible().catch(() => false)) return;
    await page.waitForTimeout(1000);
  }
  throw new Error('did not reach authenticated UI at ' + page.url());
}
async function readBack() {
  await page.goto(base + '/items', { waitUntil: 'networkidle', timeout: 60000 });
  await waitAuthed();
  await unlockIfNeeded();
  const search = page.locator('#searchWidget');
  if (await search.isVisible().catch(() => false)) {
    await search.fill(service);
    await page.waitForTimeout(1500);
  }
  const text = await page.locator('body').innerText();
  return text.includes(service) ? 1 : 0;
}
async function login() {
  await page.goto(base + '/user/login', { waitUntil: 'networkidle', timeout: 60000 });
  await page.waitForTimeout(1500);
  if (page.url().includes('/unlock')) {
    // A locked local session unlocks with the master password.
    await unlockIfNeeded();
  } else {
    await page.fill('#email', user);
    await page.fill('#password', accountPw);
    await page.click('#login-button');
  }
  await waitAuthed();
}
async function addItem() {
  await page.goto(base + '/items', { waitUntil: 'networkidle', timeout: 60000 });
  await waitAuthed();
  await page.click('#topBarQuickCreateButton');
  await page.waitForSelector('#serviceName', { state: 'visible', timeout: 30000 });
  await page.fill('#serviceName', service);
  await page.click('#quickIdentitySubmit');
  await page.waitForURL('**/items/create**', { timeout: 30000 });
  await page.waitForSelector('#service-name', { state: 'visible', timeout: 30000 });
  if (!(await page.inputValue('#service-name'))) await page.fill('#service-name', service);
  await page.fill('#username', credUser);
  await page.fill('#password', credPw);
  await page.getByRole('button', { name: 'Save Item' }).click();
  await page.waitForTimeout(3000);
}
try {
  if (mode === 'add') {
    // Login with the fixed identity, add one more credential
    // (FAKE_SERVICE names it), and read that credential back.
    await login();
    await addItem();
    const readbackA = await readBack();
    if (readbackA !== 1) throw new Error('added credential not visible');
    console.log('ok items=' + readbackA + ' readback=' + readbackA);
  } else if (mode === 'verify') {
    // Login-only: prove the account + credential exist on this instance.
    await page.goto(base + '/user/login', { waitUntil: 'networkidle', timeout: 60000 });
    await page.waitForTimeout(1500);
    if (page.url().includes('/unlock')) {
      await unlockIfNeeded();
    } else {
      await page.fill('#email', user);
      await page.fill('#password', accountPw);
      await page.click('#login-button');
    }
    await waitAuthed();
    const readbackV = await readBack();
    if (readbackV !== 1) throw new Error('credential not visible on verify login');
    console.log('ok items=' + readbackV + ' readback=' + readbackV);
  } else {
  await page.goto(base + '/user/setup', { waitUntil: 'networkidle', timeout: 60000 });
  await page.fill('#username', user);
  await page.getByRole('button', { name: 'Continue' }).click();
  await page.waitForSelector('#password', { state: 'visible', timeout: 30000 });
  await page.fill('#password', accountPw);
  await page.fill('#confirmPassword', accountPw);
  await page.getByRole('button', { name: 'Create Account' }).click();
  await waitAuthed();
  await addItem();
  const items = await readBack();
  if (items !== 1) throw new Error('credential not visible after save');
  await page.click('#userMenuButton');
  await page.waitForSelector('#userMenuLogoutButton', { state: 'visible', timeout: 15000 });
  await page.click('#userMenuLogoutButton');
  await page.waitForTimeout(2500);
  await page.goto(base + '/user/login', { waitUntil: 'networkidle', timeout: 60000 });
  await page.waitForTimeout(1500);
  if (page.url().includes('/unlock')) {
    // Logout locks the vault and keeps the local session: /user/login
    // redirects to /unlock, where the master password is the re-login.
    await unlockIfNeeded();
  } else {
    await page.fill('#email', user);
    await page.fill('#password', accountPw);
    await page.click('#login-button');
  }
  await waitAuthed();
  const readback = await readBack();
  if (readback !== 1) throw new Error('credential not visible after re-login');
  console.log('ok items=' + items + ' readback=' + readback);
  }
} catch (e) {
  console.error('error at ' + page.url() + ' :: ' + String(e && e.message || e).split('\n').slice(0, 6).join(' | '));
  process.exitCode = 1;
} finally {
  await browser.close();
}
