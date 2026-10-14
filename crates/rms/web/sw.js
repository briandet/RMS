// Service worker: lets the web UI be installed as an app and open without the network.
// The app's own files come from the network first (so a server upgrade shows at once) and
// from the cache when the server can't be reached; the API, streams and artwork are never
// cached.

const CACHE = 'rms-shell-v7';
const SHELL = [
  './', 'index.html', 'style.css', 'app.js', 'api.js', 'store.js', 'util.js', 'browser-player.js', 'pcm-worklet.js', 'pcm-output.js', 'decoders.js', 'demux.js', 'spectrum.js',
  'views/common.js', 'views/library.js', 'views/menu.js', 'views/nowplaying.js', 'views/queue.js',
  'views/search.js', 'views/sortable.js', 'views/settings.js', 'views/tags.js', 'views/track.js',
  'manifest.webmanifest', 'icon.svg', 'icon-192.png',
];
const LIVE = /\/(api|rest|stream(\.mp3)?|music|contributor|imageproxy|jsonrpc\.js|cometd)(\/|$|\?)/;

self.addEventListener('install', (e) => {
  e.waitUntil(caches.open(CACHE).then((c) => c.addAll(SHELL)).then(() => self.skipWaiting()));
});

self.addEventListener('activate', (e) => {
  e.waitUntil(caches.keys()
    .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
    .then(() => self.clients.claim()));
});

self.addEventListener('fetch', (e) => {
  const req = e.request;
  const url = new URL(req.url);
  if (req.method !== 'GET' || url.origin !== location.origin || LIVE.test(url.pathname)) return;
  e.respondWith(fetch(req).then((res) => {
    if (res.ok) {
      const copy = res.clone();
      caches.open(CACHE).then((c) => c.put(req, copy));
    }
    return res;
  }).catch(() => caches.match(req).then((hit) => hit || caches.match('index.html'))));
});
