'use strict';
'require view';
'require poll';
'require dom';
'require fs';
'require ui';

function text(value) {
	return (value === undefined || value === null || value === '') ? '-' : String(value);
}

function seconds(total) {
	var n = Number(total) || 0, days = Math.floor(n / 86400), hours = Math.floor(n % 86400 / 3600),
	    minutes = Math.floor(n % 3600 / 60), secs = n % 60;

	if (days > 0)
		return '%dd %dh %dm'.format(days, hours, minutes);
	if (hours > 0)
		return '%dh %dm'.format(hours, minutes);
	if (minutes > 0)
		return '%dm %ds'.format(minutes, secs);

	return '%ds'.format(secs);
}

function label(ok, textOn, textOff) {
	return E('span', { 'class': 'label ' + (ok ? 'success' : 'danger') }, ok ? textOn : textOff);
}

return view.extend({
	load: function() {
		return this.status();
	},

	status: function() {
		return fs.exec('/usr/libexec/iptv-status').then(function(res) {
			try {
				return JSON.parse(res.stdout || '{}');
			}
			catch (e) {
				return {};
			}
		});
	},

	restart: function() {
		return fs.exec('/etc/init.d/iptv-media-server', [ 'restart' ]);
	},

	table: function(data) {
		var running = (data.running === true || data.running === 'true'),
		    healthy = (data.health === true || data.health === 'true');

		function row(name, value) {
			return E('tr', { 'class': 'tr' }, [
				E('td', { 'class': 'td left', 'width': '33%' }, name),
				E('td', { 'class': 'td left' }, value)
			]);
		}

		return E('table', { 'class': 'table' }, [
			row(_('Service'), label(running, _('Running'), _('Stopped'))),
			row(_('Local health check'), label(healthy, _('Reachable'), _('Unreachable'))),
			row(_('Process ID'), text(data.pid)),
			row(_('Uptime'), running ? seconds(data.uptime) : '-'),
			row(_('IPTV interface'), text(data.interface)),
			row(_('Lease address'), text(data.address)),
			row(_('Lease routes'), '%s in table %s'.format(text(data.routes), text(data.route_table))),
			row(_('Policy rules'), '%s for table %s'.format(text(data.rules), text(data.route_table))),
			row(_('HTTP port'), text(data.port))
		]);
	},

	render: function(data) {
		var self = this,
		    body = E('div', {}, this.table(data || {}));

		poll.add(function() {
			return self.status().then(function(fresh) {
				dom.content(body, [ self.table(fresh) ]);
			});
		}, 5);

		return E('div', { 'class': 'cbi-map' }, [
			E('h2', {}, _('IPTV media server')),
			E('div', { 'class': 'cbi-section' }, [
				body,
				E('div', { 'class': 'cbi-page-actions' }, [
					E('button', {
						'class': 'btn cbi-button cbi-button-apply',
						'click': ui.createHandlerFn(this, function() {
							return this.restart().then(function() {
								ui.addNotification(null, E('p', {}, _('Service restarted.')), 'info');
								return self.status().then(function(fresh) {
									dom.content(body, [ self.table(fresh) ]);
								});
							});
						})
					}, _('Restart service'))
				])
			])
		]);
	}
});
