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
		return '%d 天 %d 小时 %d 分钟'.format(days, hours, minutes);
	if (hours > 0)
		return '%d 小时 %d 分钟'.format(hours, minutes);
	if (minutes > 0)
		return '%d 分钟 %d 秒'.format(minutes, secs);

	return '%d 秒'.format(secs);
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
			row(_('服务'), label(running, _('运行中'), _('已停止'))),
			row(_('本地健康检查'), label(healthy, _('可访问'), _('无法访问'))),
			row(_('进程 ID'), text(data.pid)),
			row(_('运行时间'), running ? seconds(data.uptime) : '-'),
			row(_('IPTV 接口'), text(data.interface)),
			row(_('租约地址'), text(data.address)),
			row(_('租约路由'), '%s 条，位于路由表 %s'.format(text(data.routes), text(data.route_table))),
			row(_('策略规则'), '%s 条，指向路由表 %s'.format(text(data.rules), text(data.route_table))),
			row(_('HTTP 端口'), text(data.port))
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
			E('h2', {}, _('IPTV 媒体服务器')),
			E('div', { 'class': 'cbi-section' }, [
				body,
				E('div', { 'class': 'cbi-page-actions' }, [
					E('button', {
						'class': 'btn cbi-button cbi-button-apply',
						'click': ui.createHandlerFn(this, function() {
							return this.restart().then(function() {
								ui.addNotification(null, E('p', {}, _('服务已重启。')), 'info');
								return self.status().then(function(fresh) {
									dom.content(body, [ self.table(fresh) ]);
								});
							});
						})
					}, _('重启服务'))
				])
			])
		]);
	}
});
