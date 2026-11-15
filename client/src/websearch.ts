// "Search in …" of the menu of text selected in the feed (SPEC §6): the web search a person
// picks in Settings, kept on this device.

export interface WebSearch {
  id: string;
  label: string;
  /** The results page for `q`. */
  url(q: string): string;
}

export const WEB_SEARCHES: readonly WebSearch[] = [
  { id: 'google', label: 'Google', url: (q) => `https://www.google.com/search?q=${encodeURIComponent(q)}` },
  { id: 'duckduckgo', label: 'DuckDuckGo', url: (q) => `https://duckduckgo.com/?q=${encodeURIComponent(q)}` },
  { id: 'bing', label: 'Bing', url: (q) => `https://www.bing.com/search?q=${encodeURIComponent(q)}` },
  { id: 'yandex', label: 'Yandex', url: (q) => `https://yandex.ru/search/?text=${encodeURIComponent(q)}` },
];

const KEY = 'hqc.webSearch';

/** The search picked on this device; storage that fails or holds something else means Google. */
export function storedWebSearch(): WebSearch {
  let id: string | null = null;
  try {
    id = localStorage.getItem(KEY);
  } catch {
    // The default.
  }
  return WEB_SEARCHES.find((s) => s.id === id) ?? WEB_SEARCHES[0];
}

export function keepWebSearch(s: WebSearch): void {
  try {
    localStorage.setItem(KEY, s.id);
  } catch {
    // Kept until a reload.
  }
}
