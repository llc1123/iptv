'use strict';
'require form';
'require network';

return network.registerProtocol('iptv', {
	getI18n: function() {
		return _('IPTV (authenticated DHCP)');
	},

	getIfname: function() {
		return this._ubus('l3_device') || this.get('device');
	},

	getPackageName: function() {
		return 'luci-app-iptv';
	},

	isFloating: function() {
		return false;
	},

	isVirtual: function() {
		return false;
	},

	getDevices: function() {
		return null;
	},

	containsDevice: function(ifname) {
		return (network.getIfnameOf(this.getDevice()) == ifname);
	},

	renderFormOptions: function(s) {
		var o;

		o = s.taboption('general', form.Value, 'dhcp_user', _('DHCP user'));
		o.rmempty = false;

		o = s.taboption('general', form.Value, 'dhcp_password', _('DHCP password'));
		o.password = true;
		o.rmempty = false;

		o = s.taboption('general', form.Value, 'stb_id_prefix', _('STB ID prefix'),
			_('Optional. Prepended to the DHCP hostname derived from the interface MAC.'));

	}
});
