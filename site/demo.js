for (const tab of document.querySelectorAll('[data-view]')) {
  tab.addEventListener('click', () => {
    for (const option of document.querySelectorAll('[data-view]')) {
      const active = option === tab;
      option.setAttribute('aria-pressed', String(active));
      document.getElementById(option.dataset.view).hidden = !active;
    }
  });
}

const approve = document.querySelector('#approve');
approve.addEventListener('click', () => {
  document.querySelector('#decision').textContent = 'Approved in demo';
  document.querySelector('.waiting').classList.replace('waiting', 'working');
  document.querySelector('#sessions .working').textContent = 'Working';
  approve.disabled = true;
});

const copy = document.querySelector('#copy');
if (navigator.clipboard) {
  copy.hidden = false;
  copy.addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(document.querySelector('#commands').textContent);
      copy.textContent = 'Copied';
    } catch {
      copy.textContent = 'Copy unavailable';
    }
  });
}
