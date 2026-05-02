// Playwright suite for the `wafer-run/registry` block.
//
// Scope (Task 15): exercises the *public*, unauthenticated surface against an
// empty DB plus a best-effort admin publish flow gated on `TEST_ADMIN_TOKEN`.
// Rust integration tests in `tests/registry_*.rs` cover the authenticated
// code paths end-to-end; this file only asserts the HTML + JSON wire shape
// that a real browser/CLI sees.

import { test, expect } from '@playwright/test';
import * as fs from 'node:fs';
import * as path from 'node:path';

test.describe('Registry — public', () => {
  test('public_browse_empty', async ({ page }) => {
    const resp = await page.goto('/registry');
    expect(resp?.status()).toBe(200);
    await expect(page.locator('.registry-empty h2')).toHaveText(/No packages published yet/);
  });

  test('public_package_detail_404', async ({ page }) => {
    const resp = await page.goto('/registry/acme/widget');
    expect(resp?.status()).toBe(404);
    await expect(page.locator('h1')).toHaveText('404');
  });

  test('public_search_empty_json', async ({ request }) => {
    const resp = await request.get('/registry/search');
    expect(resp.status()).toBe(200);
    const json = await resp.json();
    expect(json.total).toBe(0);
    expect(Array.isArray(json.packages)).toBe(true);
    expect(json.packages).toEqual([]);
    expect(json.query).toBe('');
  });
});

test.describe('Registry — CLI login page', () => {
  test('non_admin_cli_login_coming_soon', async ({ request }) => {
    // No session cookie, no bearer PAT — `require_user` fails and the route
    // returns 401 before the admin gate runs. (If a test-auth fixture ever
    // lands that lets us inject a non-admin session, swap this to assert 403
    // with the coming-soon banner as the plan describes.)
    const resp = await request.get('/registry/cli-login');
    expect(resp.status()).toBe(401);
  });

  // Asserting the happy-path admin CLI-login page requires an authenticated
  // admin session. The harness doesn't ship a test-only session-cookie
  // injection endpoint, so we skip here — Rust integration coverage lives in
  // `tests/registry_cli_login.rs`.
  test.skip('admin_cli_login_shows_code', async () => {
    // requires admin OAuth fixture — covered by Rust integration tests.
  });
});

test.describe('Registry — publish via direct POST', () => {
  const token = process.env.TEST_ADMIN_TOKEN;
  const fixturePath = path.join(
    __dirname,
    '_fixtures',
    'widget-0.1.0.wafer',
  );

  test('admin_publish_via_post_then_browse_appears', async ({
    page,
    request,
  }) => {
    test.skip(
      !token,
      'TEST_ADMIN_TOKEN not set — skipping admin publish round-trip.',
    );
    test.skip(
      !fs.existsSync(fixturePath),
      'widget-0.1.0.wafer fixture missing — regenerate with scripts/make-tarball.',
    );

    const tarball = fs.readFileSync(fixturePath);
    const resp = await request.post('/registry/api/publish', {
      headers: { Authorization: `Bearer ${token}` },
      multipart: {
        tarball: {
          name: 'widget-0.1.0.wafer',
          mimeType: 'application/octet-stream',
          buffer: tarball,
        },
      },
    });
    expect(resp.status()).toBe(200);

    await page.goto('/registry');
    await expect(page.locator('.packages li')).toContainText('acme/widget');
  });
});
