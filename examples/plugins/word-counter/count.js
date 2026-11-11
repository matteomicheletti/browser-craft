const text = document.body ? document.body.innerText : '';
const words = (text.match(/\S+/g) || []).length;
if (words >= bc.settings.minimum) {
  if (bc.settings.badge) {
    const b = document.createElement('div');
    b.className = 'bc-word-badge';
    b.textContent = words + ' words';
    document.documentElement.appendChild(b);
  }
  // Asynchronous, permission-checked calls to the browser.
  bc.storage.get('largest').then(function (prev) {
    if (!prev || words > prev.words) {
      return bc.storage.set('largest', { words: words, url: location.href });
    }
  }).then(function () {
    bc.log('counted ' + words + ' words');
  }).catch(function (e) {
    bc.log('storage failed: ' + e.message);
  });
}
