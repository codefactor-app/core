((root, factory) => {
  const helpers = factory();
  if (typeof module === 'object' && module.exports) module.exports = helpers;
  else root.CodefactorUI = helpers;
})(typeof globalThis === 'object' ? globalThis : this, () => {
  'use strict';
  function download(name, type, body) {
    const url = URL.createObjectURL(new Blob([body], {type}));
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = name;
    anchor.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  return {download};
});
