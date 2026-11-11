// Runs in its own function scope; `bc` is this plugin's API object.
const banner = document.createElement('div');
banner.id = 'bc-hello-banner';
banner.textContent = bc.settings.greeting + ' (' + bc.id + ' ' + bc.version + ')';
document.body.prepend(banner);
