const toggle = document.querySelector('#menu-toggle');
const sidebar = document.querySelector('#sidebar');
function closeMenu() { sidebar.classList.remove('is-open'); toggle.setAttribute('aria-expanded', 'false'); }
toggle.addEventListener('click', () => { const open = sidebar.classList.toggle('is-open'); toggle.setAttribute('aria-expanded', String(open)); });
document.addEventListener('keydown', e => { if (e.key === 'Escape') closeMenu(); });
sidebar.addEventListener('click', e => { if (e.target.closest('a')) closeMenu(); });
document.querySelector('main').addEventListener('click', closeMenu);
const toc = document.querySelector('#toc-links');
const tocDetails = document.querySelector('#toc-details');
const compactToc = matchMedia('(max-width: 1250px)');
const setTocMode = () => { tocDetails.open = !compactToc.matches; };
setTocMode();
compactToc.addEventListener('change', setTocMode);
for (const heading of document.querySelectorAll('article h2[id], article h3[id]')) {
  const link = document.createElement('a'); link.href = `#${heading.id}`; link.textContent = heading.textContent;
  if (heading.tagName === 'H3') link.className = 'subheading';
  toc.append(link);
}
if (!toc.children.length) toc.closest('.toc').hidden = true;
for (const table of document.querySelectorAll('article table')) {
  const wrapper = document.createElement('div'); wrapper.className = 'table-scroll'; wrapper.tabIndex = 0;
  table.before(wrapper); wrapper.append(table);
}
const blocks = [...document.querySelectorAll('article pre code.language-mermaid, article pre code[data-lang="mermaid"]')];
if (blocks.length) {
  try {
    const { default: mermaid } = await import('https://cdn.jsdelivr.net/npm/mermaid@11.12.0/dist/mermaid.esm.min.mjs');
    mermaid.initialize({ startOnLoad: false, securityLevel: 'strict', theme: 'neutral', fontFamily: 'system-ui, sans-serif' });
    for (const [index, code] of blocks.entries()) {
      try {
        const { svg } = await mermaid.render(`diagram-${index}`, code.textContent);
        const figure = document.createElement('figure'); figure.className = 'diagram';
        const canvas = document.createElement('div'); canvas.className = 'diagram-canvas'; canvas.tabIndex = 0; canvas.innerHTML = svg;
        const graphic = canvas.querySelector('svg');
        const width = Number(graphic.getAttribute('viewBox')?.split(/\s+/)[2]);
        if (width) { graphic.style.width = `${Math.max(400, width)}px`; graphic.style.height = 'auto'; }
        const toolbar = document.createElement('div'); toolbar.className = 'diagram-toolbar';
        const button = document.createElement('button'); button.textContent = 'Expand diagram';
        button.addEventListener('click', async () => { if (document.fullscreenElement) await document.exitFullscreen(); else await figure.requestFullscreen(); });
        document.addEventListener('fullscreenchange', () => { button.textContent = document.fullscreenElement === figure ? 'Close expanded view' : 'Expand diagram'; });
        toolbar.append(button); figure.append(toolbar, canvas);
        const pre = code.closest('pre'); const container = pre.closest('.highlighter-rouge') || pre; container.replaceWith(figure);
      } catch (error) { console.error('Diagram could not render', error); const note = document.createElement('p'); note.textContent = 'This diagram could not be rendered. Its source is shown below.'; code.closest('pre').before(note); }
    }
  } catch (error) { console.error('Mermaid could not load', error); }
}
