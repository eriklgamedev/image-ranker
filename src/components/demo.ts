import type { ScanReport } from "../native";
export function demoReport(): ScanReport {
  const colors = [
    ["#8da6aa", "#ced0b8", "#3d5150"],
    ["#ce9e78", "#ead7b1", "#765c50"],
    ["#607e98", "#b0d1cf", "#263c5f"],
    ["#83856b", "#dfe2bd", "#485343"],
    ["#9c7e7f", "#e6c3a4", "#5c536d"],
    ["#7ca5a8", "#dfe5c7", "#45676e"],
  ];
  const names = [
    "misty-ridge",
    "desert-light",
    "blue-hour",
    "quiet-valley",
    "evening-sky",
    "tide-pool",
    "highland",
    "warm-dunes",
    "distant-shore",
    "still-forest",
    "rose-mountain",
    "open-water",
  ];
  const images = names.map((name, i) => {
    const [sky, sun, mountain] = colors[i % colors.length],
      offset = (i * 17) % 90;
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="900" viewBox="0 0 1200 900"><defs><linearGradient id="s" x2="0" y2="1"><stop stop-color="${sky}"/><stop offset="1" stop-color="${sun}"/></linearGradient><linearGradient id="w" x2="0" y2="1"><stop stop-color="${sky}"/><stop offset="1" stop-color="${mountain}"/></linearGradient></defs><path fill="url(#s)" d="M0 0h1200v900H0z"/><circle cx="${820 - offset * 3}" cy="${220 + offset}" r="${70 + i * 3}" fill="${sun}" opacity=".8"/><path d="M0 660L${220 + offset} 310 560 650 840 ${420 - offset} 1200 660V900H0" fill="${mountain}" opacity=".42"/><path d="M0 750L${390 + offset} 440 740 720 1040 ${580 - offset} 1200 670V900H0" fill="${mountain}" opacity=".82"/><path fill="url(#w)" d="M0 745Q360 700 700 780T1200 745V900H0"/><path d="M0 797Q400 740 780 820T1200 790" stroke="${sun}" stroke-width="3" opacity=".3" fill="none"/></svg>`;
    return {
      id: `demo-${i}`,
      path: `Demo landscapes/${name}.jpg`,
      originalName: `${name}.jpg`,
      extension: "jpg",
      sizeBytes: 2348000 + i * 103242,
      modifiedAt: 1700000000 + i * 86400,
      width: 1200,
      height: 900,
      sourceIds: ["demo-source"],
      previewPath: `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`,
      contentRevision: `demo-${i}-1`,
    };
  });
  return {
    sources: [
      {
        id: "demo-source",
        name: "Demo landscapes",
        path: "Generated examples · no real files",
        recursive: false,
        count: images.length,
      },
    ],
    images,
    ignored: [],
  };
}
