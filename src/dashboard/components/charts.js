// charts.js — Chart.js wrappers
let ttftChart = null;
let errorsChart = null;

const chartDefaults = {
  responsive: true, maintainAspectRatio: false,
  plugins: { legend: { display: false } },
  scales: {
    x: { grid: { color: 'rgba(42,43,61,0.5)' }, ticks: { color: '#6c7086', font: { size: 10, family: 'JetBrains Mono' } } },
    y: { grid: { color: 'rgba(42,43,61,0.5)' }, ticks: { color: '#6c7086', font: { size: 10, family: 'JetBrains Mono' } }, beginAtZero: true }
  },
  animation: { duration: 300 },
  elements: { point: { radius: 0 }, line: { tension: 0.3, borderWidth: 2 } }
};

function initCharts() {
  const ttftCtx = document.getElementById('chart-ttft').getContext('2d');
  ttftChart = new Chart(ttftCtx, {
    type: 'line',
    data: { labels: [], datasets: [{ data: [], borderColor: '#89dceb', backgroundColor: 'rgba(137,220,235,0.1)', fill: true }] },
    options: { ...chartDefaults, scales: { ...chartDefaults.scales, y: { ...chartDefaults.scales.y, title: { display: true, text: 'ms', color: '#6c7086' } } } }
  });

  const errCtx = document.getElementById('chart-errors').getContext('2d');
  errorsChart = new Chart(errCtx, {
    type: 'bar',
    data: {
      labels: [],
      datasets: [
        { label: 'Requests', data: [], backgroundColor: 'rgba(137,180,250,0.4)', borderRadius: 3, order: 2 },
        { label: 'Errors', data: [], backgroundColor: 'rgba(243,139,168,0.7)', borderRadius: 3, order: 1 }
      ]
    },
    options: { ...chartDefaults, plugins: { legend: { display: true, labels: { color: '#6c7086', font: { size: 10 } } } } }
  });
}

function updateCharts() {
  if (!statsSnapshot || !ttftChart) return;

  const labels = statsSnapshot.stats.ttft_timeseries.map(p => {
    const d = new Date(p.timestamp);
    return d.getHours().toString().padStart(2, '0') + ':' + d.getMinutes().toString().padStart(2, '0');
  });

  ttftChart.data.labels = labels;
  ttftChart.data.datasets[0].data = statsSnapshot.stats.ttft_timeseries.map(p => p.value > 0 ? p.value.toFixed(0) : null);
  ttftChart.update('none');

  errorsChart.data.labels = labels;
  errorsChart.data.datasets[0].data = statsSnapshot.stats.request_timeseries.map(p => p.value);
  errorsChart.data.datasets[1].data = statsSnapshot.stats.error_timeseries.map(p => p.value);
  errorsChart.update('none');
}

function resizeCharts() {
  if (ttftChart) ttftChart.resize();
  if (errorsChart) errorsChart.resize();
}

function resetCharts() {
  if (ttftChart) {
    ttftChart.data.labels = [];
    ttftChart.data.datasets[0].data = [];
    ttftChart.update('none');
  }

  if (errorsChart) {
    errorsChart.data.labels = [];
    errorsChart.data.datasets[0].data = [];
    errorsChart.data.datasets[1].data = [];
    errorsChart.update('none');
  }
}
