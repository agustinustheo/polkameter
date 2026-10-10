// Renders ```mermaid blocks (turned into <pre class="mermaid"> by mdbook-mermaid) with Mermaid
// from a CDN, in the dark theme when the book uses a dark one.
(async () => {
	if (!document.querySelector('.mermaid')) return;
	const { default: mermaid } = await import('https://cdn.jsdelivr.net/npm/mermaid@11.12.0/dist/mermaid.esm.min.mjs');
	const dark = ['ayu', 'navy', 'coal'].some((t) => document.documentElement.classList.contains(t));
	mermaid.initialize({ startOnLoad: false, theme: dark ? 'dark' : 'default' });
	await mermaid.run();
	for (const id of ['ayu', 'navy', 'coal', 'light', 'rust']) {
		document.getElementById(id)?.addEventListener('click', () => window.location.reload());
	}
})();
