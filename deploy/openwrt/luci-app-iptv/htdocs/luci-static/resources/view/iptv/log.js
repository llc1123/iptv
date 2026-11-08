'use strict';
'require view';
'require poll';
'require dom';
'require fs';
'require ui';

return view.extend({
	lines: 300,

	fetch: function() {
		return fs.exec('/usr/libexec/iptv-log', [ String(this.lines) ]).then(function(res) {
			return (res && res.stdout) ? res.stdout : '';
		});
	},

	render: function() {
		var self = this,
		    count = E('span', {}),
		    output = E('pre', {
			'style': 'max-height: 70vh; overflow: auto; margin: 0; font-size: 12px; white-space: pre-wrap; word-break: break-all;'
		}, _('Loading…'));

		function refresh() {
			return self.fetch().then(function(body) {
				var trimmed = body.replace(/\s+$/, '');
				dom.content(output, [ trimmed || _('No log entries yet.') ]);
				dom.content(count, [ '%s'.format(trimmed ? trimmed.split('\n').length : 0) ]);
			});
		}

		poll.add(refresh, 10);

		return E('div', { 'class': 'cbi-map' }, [
			E('h2', {}, _('IPTV media server')),
			E('div', { 'class': 'cbi-section' }, [
				E('div', { 'class': 'cbi-section-node' }, [
					E('p', {}, [
						_('Last %s lines of the system log for this service.').format(this.lines),
						' ',
						E('span', {}, [ count, ' ', _('lines') ])
					]),
					output,
					E('div', { 'class': 'cbi-page-actions' }, [
						E('button', {
							'class': 'btn cbi-button cbi-button-action',
							'click': ui.createHandlerFn(this, function() {
								return refresh();
							})
						}, _('Refresh'))
					])
				])
			])
		]);
	},

	handleSaveApply: null,
	handleSave: null
});
