// @vitest-environment jsdom
import { StrictMode } from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { HelpDialog } from "../../src/components/HelpDialog";
import type { AppInfo, LicenseNotice, StorageLocation } from "../../src/native";

const api = vi.hoisted(() => ({
  appInfo: vi.fn(),
  licenseNotices: vi.fn(),
  storageLocations: vi.fn(),
  imageWorkStatus: vi.fn(),
}));
vi.mock("../../src/native", () => ({ desktop: true, api }));
const info: AppInfo = {
  name: "Image Ranker",
  version: "1.0-current",
  platform: "windows",
  architecture: "x86_64",
  buildType: "debug",
  testDriverEnabled: true,
  applicationLicense: null,
  decode: {
    formats: ["PNG"],
    maxInputBytes: 1024,
    maxPixels: 2048,
    maxEdge: 128,
    maxDecodeBytes: 8192,
    maxIccBytes: 512,
    channelBits: [8],
    gif: "first-frame",
    tiff: "first-page",
    orientation: "exif-normalized",
    color: "embedded-profile-to-srgb",
  },
};
const notices: LicenseNotice[] = [
  {
    id: "example",
    component: "Bundled library",
    version: "1.0",
    spdx: "MIT",
    text: "Exact bundled notice text.",
  },
];
const locations: StorageLocation[] = [
  {
    kind: "profile",
    label: "Current profile data",
    displayPath: "C:\\current-profile",
    available: true,
  },
];
function deferred<T>() {
  let resolve!: (value: T) => void, reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
async function show(strict = false) {
  let view!: ReturnType<typeof render>;
  await act(async () => {
    const dialog = <HelpDialog onClose={() => {}} />;
    view = render(strict ? <StrictMode>{dialog}</StrictMode> : dialog);
  });
  return view;
}
beforeEach(() => {
  vi.resetAllMocks();
  api.appInfo.mockResolvedValue(info);
  api.licenseNotices.mockResolvedValue(notices);
  api.storageLocations.mockResolvedValue(locations);
  api.imageWorkStatus.mockResolvedValue({
    active: 0,
    limit: 18,
    jobs: [],
    decoder: {
      active: 0,
      pending: 0,
      reservedBytes: 0,
      liveOutputs: 0,
      outputReservedBytes: 0,
      unavailable: false,
      workerPid: null,
      stage: null,
      recoveryAction: null,
    },
  });
});
afterEach(cleanup);

it("publishes bundled version and notices while recovery locations remain unresolved", async () => {
  const storage = deferred<StorageLocation[]>();
  api.storageLocations.mockReturnValue(storage.promise);
  await show();
  expect(screen.getByText(/Version 1.0-current/)).toBeTruthy();
  expect(screen.getByText("Exact bundled notice text.")).toBeTruthy();
  expect(screen.getByText(/Loading local support information/)).toBeTruthy();
  expect(
    screen.queryByRole("button", { name: "Open Current profile data" }),
  ).toBeNull();
  expect(
    (screen.getByRole("button", { name: "Done" }) as HTMLButtonElement)
      .disabled,
  ).toBe(false);
  await act(async () => storage.resolve(locations));
  expect(
    screen.getByRole("button", { name: "Open Current profile data" }),
  ).toBeTruthy();
  expect(screen.queryByText(/Loading local support information/)).toBeNull();
});

it("retries only failed metadata without duplicating a pending location lookup or hiding available notices", async () => {
  const storage = deferred<StorageLocation[]>(),
    retry = deferred<AppInfo>();
  api.storageLocations.mockReturnValue(storage.promise);
  api.appInfo
    .mockRejectedValueOnce(new Error("APP_BUSY: retry details"))
    .mockReturnValueOnce(retry.promise);
  await show();
  expect(screen.getByText(/Application details: Error: APP_BUSY/)).toBeTruthy();
  expect(screen.getByText("Exact bundled notice text.")).toBeTruthy();
  await act(async () => {
    fireEvent.click(
      screen.getByRole("button", { name: "Retry support information" }),
    );
  });
  expect(api.appInfo).toHaveBeenCalledTimes(2);
  expect(api.storageLocations).toHaveBeenCalledTimes(1);
  expect(api.licenseNotices).toHaveBeenCalledTimes(1);
  expect(screen.queryByText(/Application details: Error: APP_BUSY/)).toBeNull();
  expect(
    screen.queryByRole("button", { name: "Retry support information" }),
  ).toBeNull();
  await act(async () => retry.resolve(info));
  expect(screen.getByText(/Version 1.0-current/)).toBeTruthy();
  await act(async () =>
    storage.reject(new Error("LOCATION_BUSY: still unavailable")),
  );
  expect(
    screen.getByText(/Recovery locations: Error: LOCATION_BUSY/),
  ).toBeTruthy();
  expect(screen.getByText(/Version 1.0-current/)).toBeTruthy();
});

it("retains separate location and notice errors as other metadata succeeds", async () => {
  const app = deferred<AppInfo>(),
    license = deferred<LicenseNotice[]>();
  api.appInfo.mockReturnValue(app.promise);
  api.licenseNotices.mockReturnValue(license.promise);
  api.storageLocations.mockRejectedValue(
    new Error("LOCATION_UNAVAILABLE: preserved"),
  );
  await show();
  expect(
    screen.getByText(/Recovery locations: Error: LOCATION_UNAVAILABLE/),
  ).toBeTruthy();
  await act(async () =>
    license.reject(new Error("NOTICE_UNAVAILABLE: preserved")),
  );
  expect(
    screen.getByText(/License notices: Error: NOTICE_UNAVAILABLE/),
  ).toBeTruthy();
  await act(async () => app.resolve(info));
  expect(screen.getByText(/Version 1.0-current/)).toBeTruthy();
  expect(
    screen.getByText(/Recovery locations: Error: LOCATION_UNAVAILABLE/),
  ).toBeTruthy();
  expect(
    screen.getByText(/License notices: Error: NOTICE_UNAVAILABLE/),
  ).toBeTruthy();
  expect(screen.queryByText(/No third-party notices were supplied/)).toBeNull();
});

it("uses only the notice result to distinguish pending, unavailable, and an actually empty notice list", async () => {
  const license = deferred<LicenseNotice[]>(),
    storage = deferred<StorageLocation[]>();
  api.licenseNotices.mockReturnValue(license.promise);
  api.storageLocations.mockReturnValue(storage.promise);
  api.appInfo.mockRejectedValue(new Error("APP_UNAVAILABLE: details missing"));
  await show();
  expect(screen.queryByText(/No third-party notices were supplied/)).toBeNull();
  await act(async () => license.resolve([]));
  expect(screen.getByText(/No third-party notices were supplied/)).toBeTruthy();
  expect(
    screen.getByText(/Application details: Error: APP_UNAVAILABLE/),
  ).toBeTruthy();
  expect(screen.getByText(/Loading local support information/)).toBeTruthy();
});

it("ignores older StrictMode metadata responses after the active effect has published", async () => {
  const oldApp = deferred<AppInfo>(),
    oldNotices = deferred<LicenseNotice[]>(),
    oldStorage = deferred<StorageLocation[]>();
  api.appInfo.mockReturnValueOnce(oldApp.promise);
  api.licenseNotices.mockReturnValueOnce(oldNotices.promise);
  api.storageLocations.mockReturnValueOnce(oldStorage.promise);
  await show(true);
  expect(screen.getByText(/Version 1.0-current/)).toBeTruthy();
  await act(async () => {
    oldApp.resolve({ ...info, version: "0.0-obsolete" });
    oldNotices.reject(new Error("OLD_NOTICE_ERROR"));
    oldStorage.resolve([{ ...locations[0], label: "Obsolete profile data" }]);
  });
  expect(screen.getByText(/Version 1.0-current/)).toBeTruthy();
  expect(screen.getByText("Exact bundled notice text.")).toBeTruthy();
  expect(
    screen.getByRole("button", { name: "Open Current profile data" }),
  ).toBeTruthy();
  expect(
    screen.queryByText(/0.0-obsolete|OLD_NOTICE_ERROR|Obsolete profile data/),
  ).toBeNull();
});

it("ignores unmounted support results when a new Help dialog has current metadata", async () => {
  const oldApp = deferred<AppInfo>(),
    oldNotices = deferred<LicenseNotice[]>(),
    oldStorage = deferred<StorageLocation[]>();
  api.appInfo.mockReturnValueOnce(oldApp.promise);
  api.licenseNotices.mockReturnValueOnce(oldNotices.promise);
  api.storageLocations.mockReturnValueOnce(oldStorage.promise);
  const previous = await show();
  previous.unmount();
  await show();
  await act(async () => {
    oldApp.reject(new Error("OLD_APP_ERROR"));
    oldNotices.resolve([{ ...notices[0], text: "Obsolete notice text." }]);
    oldStorage.reject(new Error("OLD_STORAGE_ERROR"));
  });
  expect(screen.getByText(/Version 1.0-current/)).toBeTruthy();
  expect(screen.getByText("Exact bundled notice text.")).toBeTruthy();
  expect(
    screen.getByRole("button", { name: "Open Current profile data" }),
  ).toBeTruthy();
  expect(
    screen.queryByText(/OLD_APP_ERROR|OLD_STORAGE_ERROR|Obsolete notice text/),
  ).toBeNull();
});
