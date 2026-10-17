'use strict';
/* Compact controls own DOM state; the map controller owns analysis and selection. */
window.CodefactorMapControls = {
  create({state, change, action}) {
    const $ = id => document.getElementById(id);
    const structureButtons = [...document.querySelectorAll('[data-map-structure]')];

    function sync() {
      const ui = state();
      const fullscreen=document.body.classList.contains('map-fullscreen');
      $('map-fullscreen').setAttribute('aria-pressed',String(fullscreen));
      $('map-fullscreen').title=fullscreen?'Вернуться к обычному виду':'Развернуть карту на весь экран';
      for (const button of structureButtons) {
        const active = ui.group === button.dataset.mapStructure;
        button.classList.toggle('active', active);
        button.setAttribute('aria-pressed', String(active));
      }
      const extra = ['file', 'set'].includes(ui.group);
      $('map-extra-group').value = extra ? ui.group : '';
      $('map-extra-active').classList.toggle('hidden', !extra);
      $('map-extra-active').textContent = ui.group === 'set' ? 'Наборы ×' : 'Все файлы ×';
      for (const key of ['view', 'direction', 'arrangement']) $('map-' + key).value = ui[key];
      $('map-direction').disabled = !ui.selected;
      $('map-search').value = ui.search;
      $('map-external').checked = ui.external;
      for (const kind of CodefactorMapModel.kinds) $('map-kind-' + kind).checked = ui.kinds.includes(kind);
      const count = Number(extra) + Number(ui.external) + Number(ui.direction !== 'both')
        + CodefactorMapModel.kinds.filter(kind => !ui.kinds.includes(kind)).length;
      $('map-options-count').classList.toggle('hidden', count === 0);
      $('map-options-count').textContent = String(count);
      document.querySelector('.map-canvas-tools').classList.toggle('hidden', ui.view === 'matrix');
    }

    for (const button of structureButtons) button.onclick = () => change('group', button.dataset.mapStructure);
    $('map-extra-group').onchange = event => change('group', event.target.value || state().structure);
    $('map-extra-active').onclick = () => change('group', state().structure);
    for (const key of ['view', 'direction', 'arrangement']) $('map-' + key).onchange = event => change(key, event.target.value);
    $('map-search').oninput = event => change('search', event.target.value);
    $('map-external').onchange = event => change('external', event.target.checked);
    for (const kind of CodefactorMapModel.kinds) $('map-kind-' + kind).onchange = () => {
      change('kinds', CodefactorMapModel.kinds.filter(value => $('map-kind-' + value).checked));
    };
    for (const name of ['overview', 'collapse', 'zoom-in', 'zoom-out', 'zoom-value', 'layout', 'fullscreen']) $('map-' + name).onclick = () => action(name);
    for (const format of ['json', 'svg', 'png']) $('map-export-' + format).onclick = () => action('export', format);

    // Settings overlay the map, so opening them does not shrink its viewport.
    document.addEventListener('click', event => {
      if (!event.target.closest('#map-options')) $('map-options').open = false;
    });
    $('map-options').addEventListener('keydown', event => {
      if (event.key === 'Escape') {
        event.preventDefault();
        $('map-options').open = false;
        $('map-options').querySelector('summary').focus();
      }
    });

    function resize() {
      const stage = document.querySelector('.codemap-stage');
      if ($('page-codemap').classList.contains('hidden')) return;
      const footer = document.querySelector('.map-footer');
      stage.style.height = Math.max(260, window.innerHeight - stage.getBoundingClientRect().top - footer.offsetHeight - 16) + 'px';
      action('resize');
    }
    const observer = new ResizeObserver(resize);
    observer.observe(document.querySelector('.map-toolbar'));
    observer.observe(document.querySelector('.map-footer'));
    window.addEventListener('resize', resize);
    document.addEventListener('keydown',event=>{if(event.key==='Escape'&&document.body.classList.contains('map-fullscreen')){action('fullscreen');}});

    function setNamedSets(sets) {
      const names = Object.keys(sets);
      $('map-named-sets').textContent = names.length
        ? 'Наборы из конфигурации: ' + names.join(', ') + '.'
        : 'Наборы пока не заданы. Добавьте раздел sets в конфигурацию проекта.';
    }
    sync();
    return {sync, resize, setNamedSets};
  }
};
