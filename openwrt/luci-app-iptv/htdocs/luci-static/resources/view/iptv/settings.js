'use strict';
'require form';
'require ui';
'require view';

return view.extend({
	render: function() {
		var m, s, o;

		m = new form.Map('iptv', _('IPTV media server'),
			_('The server acquires its lease on the IPTV interface and keeps that network out of the host routing table.'));

		s = m.section(form.NamedSection, 'main', 'server', _('General'));
		s.anonymous = true;
		s.addremove = false;

		o = s.option(form.Flag, 'enabled', _('Enable'));
		o.default = '1';
		o.rmempty = false;

		o = s.option(form.Flag, 'managed', _('Managed by netifd'),
			_('Take the address from a network interface using the iptv protocol instead of acquiring a lease here. That interface must be named iptv, because the service reads its address and reacts to its events by name.'));
		o.default = '0';
		o.rmempty = false;

		o = s.option(form.Value, 'interface', _('IPTV interface'),
			_('Interface that reaches the provider, for example eth1 on a router with a dedicated IPTV port.'));
		o.default = 'eth1';
		o.rmempty = false;
		o.placeholder = 'eth1';

		o = s.option(form.Value, 'host', _('Listen address'));
		o.default = '0.0.0.0';
		o.rmempty = false;

		o = s.option(form.Value, 'port', _('Listen port'));
		o.datatype = 'port';
		o.default = '8080';
		o.rmempty = false;

		o = s.option(form.Value, 'route_table', _('Routing table'),
			_('Table that carries the IPTV subnet, a default route and the matching rule. Must stay below the main table (32766). Ignored when netifd manages the interface, where `option ip4table` on the IPTV interface decides.'));
		o.datatype = 'range(1,32765)';
		o.validate = function(section_id, value) {
			var n = Number(value);

			// The server's parser rejects these too; failing here keeps procd
			// from respawning a server that cannot start.
			if (n >= 253 && n <= 255)
				return _('Tables 253-255 are reserved by the kernel.');

			return true;
		};
		o.default = '4788';
		o.rmempty = false;

		s = m.section(form.NamedSection, 'main', 'server', _('Credentials'));
		s.anonymous = true;
		s.addremove = false;

		o = s.option(form.Value, 'dhcp_user', _('DHCP user'));
		o.rmempty = false;
		// netifd's iptv interface holds these in managed mode.
		o.depends('managed', '0');

		o = s.option(form.Value, 'dhcp_password', _('DHCP password'));
		o.password = true;
		o.rmempty = false;
		// netifd's iptv interface holds these in managed mode.
		o.depends('managed', '0');

		o = s.option(form.Value, 'epg_user', _('EPG user'));
		o.rmempty = false;

		o = s.option(form.Value, 'epg_password', _('EPG password'));
		o.password = true;
		o.rmempty = false;

		o = s.option(form.Value, 'epg_mac', _('STB MAC address'),
			_('Optional. Defaults to the MAC address of the IPTV interface.'));
		o.datatype = 'macaddr';
		o.placeholder = '00:00:00:00:00:00';

		o = s.option(form.Value, 'stb_id_prefix', _('STB ID prefix'),
			_('Optional. Prepended to the DHCP hostname derived from the interface MAC.'));

		o = s.option(form.Value, 'authentication_url', _('Authentication URL'));
		o.placeholder = 'http://host:port/EDS/jsp/AuthenticationURL';

		s = m.section(form.NamedSection, 'main', 'server', _('Guide'));
		s.anonymous = true;
		s.addremove = false;

		o = s.option(form.Value, 'guide_past_days', _('Past days'));
		o.datatype = 'range(0,30)';
		o.default = '7';

		o = s.option(form.Value, 'guide_future_days', _('Future days'));
		o.datatype = 'range(0,30)';
		o.default = '2';

		o = s.option(form.Value, 'guide_timezone', _('Guide time zone'));
		o.default = 'Asia/Shanghai';
		o.rmempty = false;

		o = s.option(form.Value, 'refresh_interval_seconds', _('Refresh interval'),
			_('Seconds between guide refreshes.'));
		o.datatype = 'uinteger';
		o.default = '3600';
		o.rmempty = false;

		o = s.option(form.Value, 'guide_cache', _('Guide cache'),
			_('Warm-start file. Keep it on tmpfs to avoid writing the guide to flash every hour.'));
		o.default = '/var/lib/iptv/guide.json';
		o.rmempty = false;

		o = s.option(form.Value, 'logo_dir', _('Logo directory'),
			_('Directory holding one PNG per channel, named after the channel ID.'));
		o.default = '/etc/iptv/logos';
		o.rmempty = false;

		return m.render();
	}
});
