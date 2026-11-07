"""Locale support: access to the POSIX locale database and locale-aware
number formatting and string collation."""

import sys
import encodings
import encodings.aliases
import _collections_abc
from builtins import str as _builtin_str
import functools

__all__ = ["getlocale", "getdefaultlocale", "getpreferredencoding", "Error",
           "setlocale", "localeconv", "strcoll", "strxfrm",
           "str", "atof", "atoi", "format_string", "currency",
           "normalize", "LC_CTYPE", "LC_COLLATE", "LC_TIME", "LC_MONETARY",
           "LC_NUMERIC", "LC_ALL", "CHAR_MAX", "getencoding"]


def _strcoll(a, b):
    """Compare two strings (C-locale fallback)."""
    return (a > b) - (a < b)


def _strxfrm(s):
    """Return a sort key for s (C-locale fallback)."""
    return s


try:
    from _locale import *
except ImportError:
    # Emulation of the C locale only.
    CHAR_MAX = 127
    LC_ALL = 6
    LC_COLLATE = 3
    LC_CTYPE = 0
    LC_MESSAGES = 5
    LC_MONETARY = 4
    LC_NUMERIC = 1
    LC_TIME = 2
    Error = ValueError

    def localeconv():
        """Numeric formatting parameters of the C locale."""
        return {'grouping': [127],
                'currency_symbol': '',
                'n_sign_posn': 127,
                'p_cs_precedes': 127,
                'n_cs_precedes': 127,
                'mon_grouping': [],
                'n_sep_by_space': 127,
                'decimal_point': '.',
                'negative_sign': '',
                'positive_sign': '',
                'p_sep_by_space': 127,
                'int_curr_symbol': '',
                'p_sign_posn': 127,
                'thousands_sep': '',
                'mon_thousands_sep': '',
                'frac_digits': 127,
                'mon_decimal_point': '',
                'int_frac_digits': 127}

    def setlocale(category, value=None):
        """Only the C locale is available."""
        if value not in (None, '', 'C'):
            raise Error('_locale emulation only supports "C" locale')
        return 'C'

if 'strxfrm' not in globals():
    strxfrm = _strxfrm
if 'strcoll' not in globals():
    strcoll = _strcoll


_localeconv = localeconv

# Test hook: values that override localeconv() entries.
_override_localeconv = {}


@functools.wraps(_localeconv)
def localeconv():
    d = _localeconv()
    if _override_localeconv:
        d.update(_override_localeconv)
    return d


### Number formatting APIs

def _grouping_intervals(grouping):
    last_interval = None
    for interval in grouping:
        # CHAR_MAX: no further grouping
        if interval == CHAR_MAX:
            return
        # 0: repeat the last interval
        if interval == 0:
            if last_interval is None:
                raise ValueError("invalid grouping")
            while True:
                yield last_interval
        yield interval
        last_interval = interval


def _group(s, monetary=False):
    conv = localeconv()
    thousands_sep = conv[monetary and 'mon_thousands_sep' or 'thousands_sep']
    grouping = conv[monetary and 'mon_grouping' or 'grouping']
    if not grouping:
        return (s, 0)
    if s[-1] == ' ':
        stripped = s.rstrip()
        right_spaces = s[len(stripped):]
        s = stripped
    else:
        right_spaces = ''
    left_spaces = ''
    groups = []
    for interval in _grouping_intervals(grouping):
        if not s or s[-1] not in "0123456789":
            # only non-digit characters remain (sign, spaces)
            left_spaces = s
            s = ''
            break
        groups.append(s[-interval:])
        s = s[:-interval]
    if s:
        groups.append(s)
    groups.reverse()
    return (
        left_spaces + thousands_sep.join(groups) + right_spaces,
        len(thousands_sep) * (len(groups) - 1)
    )


def _strip_padding(s, amount):
    """Remove up to amount spaces of padding (added by grouping)."""
    lpos = 0
    while amount and s[lpos] == ' ':
        lpos += 1
        amount -= 1
    rpos = len(s) - 1
    while amount and s[rpos] == ' ':
        rpos -= 1
        amount -= 1
    return s[lpos:rpos + 1]


_percent_re = None


def _get_percent_re():
    global _percent_re
    if _percent_re is None:
        import re
        _percent_re = re.compile(r'%(?:\((?P<key>.*?)\))?(?P<modifiers'
                                 r'>[-#0-9 +*.hlL]*?)[eEfFgGdiouxXcrs%]')
    return _percent_re


def _format(percent, value, grouping=False, monetary=False, *additional):
    if additional:
        formatted = percent % ((value,) + additional)
    else:
        formatted = percent % value
    if percent[-1] in 'eEfFgGdiu':
        formatted = _localize(formatted, grouping, monetary)
    return formatted


def _localize(formatted, grouping=False, monetary=False):
    # Use the locale's decimal point and thousands separator.
    if '.' in formatted:
        seps = 0
        parts = formatted.split('.')
        if grouping:
            parts[0], seps = _group(parts[0], monetary=monetary)
        decimal_point = localeconv()[monetary and 'mon_decimal_point'
                                     or 'decimal_point']
        formatted = decimal_point.join(parts)
        if seps:
            formatted = _strip_padding(formatted, seps)
    else:
        seps = 0
        if grouping:
            formatted, seps = _group(formatted, monetary=monetary)
        if seps:
            formatted = _strip_padding(formatted, seps)
    return formatted


def format_string(f, val, grouping=False, monetary=False):
    """Format f % val with locale-aware number formatting (thousands
    grouping with grouping=True, monetary separators with monetary=True)."""
    percents = list(_get_percent_re().finditer(f))
    new_f = _get_percent_re().sub('%s', f)

    if isinstance(val, _collections_abc.Mapping):
        new_val = []
        for perc in percents:
            if perc.group()[-1] == '%':
                new_val.append('%')
            else:
                new_val.append(_format(perc.group(), val, grouping, monetary))
    else:
        if not isinstance(val, tuple):
            val = (val,)
        new_val = []
        i = 0
        for perc in percents:
            if perc.group()[-1] == '%':
                new_val.append('%')
            else:
                starcount = perc.group('modifiers').count('*')
                new_val.append(_format(perc.group(),
                                       val[i],
                                       grouping,
                                       monetary,
                                       *val[i + 1:i + 1 + starcount]))
                i += (1 + starcount)
    val = tuple(new_val)

    return new_f % val


def currency(val, symbol=True, grouping=False, international=False):
    """Format a number as a currency amount according to the locale."""
    conv = localeconv()

    # check for illegal values
    digits = conv[international and 'int_frac_digits' or 'frac_digits']
    if digits == 127:
        raise ValueError("Currency formatting is not possible using "
                         "the 'C' locale.")

    s = _localize(f'{abs(val):.{digits}f}', grouping, monetary=True)
    # The symbol goes before or after the amount, possibly with a space.
    s = '<' + s + '>'

    if symbol:
        smb = conv[international and 'int_curr_symbol' or 'currency_symbol']
        precedes = conv[val < 0 and 'n_cs_precedes' or 'p_cs_precedes']
        separated = conv[val < 0 and 'n_sep_by_space' or 'p_sep_by_space']

        if precedes:
            s = smb + (separated and ' ' or '') + s
        else:
            if international and smb[-1] == ' ':
                smb = smb[:-1]
            s = s + (separated and ' ' or '') + smb

    sign_pos = conv[val < 0 and 'n_sign_posn' or 'p_sign_posn']
    sign = conv[val < 0 and 'negative_sign' or 'positive_sign']

    if sign_pos == 0:
        s = '(' + s + ')'
    elif sign_pos == 1:
        s = sign + s
    elif sign_pos == 2:
        s = s + sign
    elif sign_pos == 3:
        s = s.replace('<', sign)
    elif sign_pos == 4:
        s = s.replace('>', sign)
    else:
        # the default if nothing specified;
        # this should be the most fitting sign position
        s = sign + s

    return s.replace('<', '').replace('>', '')


def str(val):
    """Convert float to string, taking the locale into account."""
    return _format("%.12g", val)


def delocalize(string):
    "Parses a string as a normalized number according to the locale settings."

    conv = localeconv()

    # First, get rid of the grouping
    ts = conv['thousands_sep']
    if ts:
        string = string.replace(ts, '')

    # next, replace the decimal point with a dot
    dd = conv['decimal_point']
    if dd:
        string = string.replace(dd, '.')
    return string


def localize(string, grouping=False, monetary=False):
    """Parses a string as locale number according to the locale settings."""
    return _localize(string, grouping, monetary)


def atof(string, func=float):
    "Parses a string as a float according to the locale settings."
    return func(delocalize(string))


def atoi(string):
    "Converts a string to an integer according to the locale settings."
    return int(delocalize(string))


def _test():
    setlocale(LC_ALL, "")
    s1 = format_string("%d", 123456789, 1)
    print(s1, "is", atoi(s1))
    s1 = str(3.14)
    print(s1, "is", atof(s1))


### Locale name aliasing engine

_setlocale = setlocale


def _replace_encoding(code, encoding):
    if '.' in code:
        langname = code[:code.index('.')]
    else:
        langname = code
    # Map the encoding to the spelling used by the C library.
    norm_encoding = encodings.normalize_encoding(encoding)
    norm_encoding = encodings.aliases.aliases.get(norm_encoding.lower(),
                                                  norm_encoding)
    encoding = norm_encoding
    norm_encoding = norm_encoding.lower()
    if norm_encoding in locale_encoding_alias:
        encoding = locale_encoding_alias[norm_encoding]
    else:
        norm_encoding = norm_encoding.replace('_', '')
        norm_encoding = norm_encoding.replace('-', '')
        if norm_encoding in locale_encoding_alias:
            encoding = locale_encoding_alias[norm_encoding]
    return langname + '.' + encoding


def _append_modifier(code, modifier):
    if modifier == 'euro':
        # The euro modifier implies ISO8859-15 for Latin-1 locales and is
        # meaningless for UTF-8 ones.
        if '.' not in code:
            return code + '.ISO8859-15@euro'
        _, _, encoding = code.partition('.')
        if encoding == 'UTF-8':
            return code
        if encoding == 'ISO8859-1':
            return _replace_encoding(code, 'ISO8859-15') + '@euro'
    return code + '@' + modifier


def normalize(localename):
    """Return a normalized locale code for localename (e.g. 'en_us' ->
    'en_US.ISO8859-1'); unknown names are returned unchanged."""
    code = localename.lower()
    if ':' in code:
        # ':' is sometimes used as encoding delimiter.
        code = code.replace(':', '.')
    if '@' in code:
        code, modifier = code.split('@', 1)
    else:
        modifier = ''
    if '.' in code:
        langname, encoding = code.split('.')[:2]
    else:
        langname = code
        encoding = ''

    # 1. language + encoding + modifier
    lang_enc = langname
    if encoding:
        norm_encoding = encoding.replace('-', '')
        norm_encoding = norm_encoding.replace('_', '')
        lang_enc += '.' + norm_encoding
    lookup_name = lang_enc
    if modifier:
        lookup_name += '@' + modifier
    code = locale_alias.get(lookup_name, None)
    if code is not None:
        return code

    if modifier:
        # 2. language + encoding
        code = locale_alias.get(lang_enc, None)
        if code is not None:
            if '@' not in code:
                return _append_modifier(code, modifier)
            if code.split('@', 1)[1].lower() == modifier:
                return code

    if encoding:
        # 3. language + modifier, with the encoding substituted
        lookup_name = langname
        if modifier:
            lookup_name += '@' + modifier
        code = locale_alias.get(lookup_name, None)
        if code is not None:
            if '@' not in code:
                return _replace_encoding(code, encoding)
            code, modifier = code.split('@', 1)
            return _append_modifier(_replace_encoding(code, encoding), modifier)

        if modifier:
            # 4. language only
            code = locale_alias.get(langname, None)
            if code is not None:
                if '@' not in code:
                    code = _replace_encoding(code, encoding)
                    return _append_modifier(code, modifier)
                code, defmod = code.split('@', 1)
                if defmod.lower() == modifier:
                    return _replace_encoding(code, encoding) + '@' + defmod

    return localename


def _parse_localename(localename):
    """Split a locale name into (language code, encoding); both may be
    None (the C locale)."""
    code = normalize(localename)
    if '@' in code:
        # Deal with locale modifiers
        code, modifier = code.split('@', 1)
        if modifier == 'euro' and '.' not in code:
            # Assume ISO8859-15 for the euro modifier.
            return code + '@euro', 'ISO8859-15'
    else:
        modifier = ''

    if '.' in code:
        langname, encoding = code.split('.')[:2]
        if modifier:
            langname += '@' + modifier
        return langname, encoding
    elif code == 'C':
        return None, None
    elif code == 'UTF-8':
        # On macOS "LC_CTYPE=UTF-8" is a valid locale setting
        # for getting UTF-8 handling for text.
        return None, 'UTF-8'
    raise ValueError('unknown locale: %s' % localename)


def _build_localename(localetuple):
    """Build a locale name from a (language code, encoding) tuple."""
    try:
        language, encoding = localetuple

        if language is None:
            language = 'C'
        if encoding is None:
            return language
        if '@' in language:
            language, modifier = language.split('@', 1)
            return language + '.' + encoding + '@' + modifier
        return language + '.' + encoding
    except (TypeError, ValueError):
        raise TypeError('Locale must be None, a string, or an iterable of '
                        'two strings -- language code, encoding.') from None


def getdefaultlocale(envvars=('LC_ALL', 'LC_CTYPE', 'LANG', 'LANGUAGE')):
    """Try to determine the default locale settings.

    Returns (language code, encoding) from the first set environment
    variable of envvars."""
    return _getdefaultlocale(envvars)


def _getdefaultlocale(envvars=('LC_ALL', 'LC_CTYPE', 'LANG', 'LANGUAGE')):
    try:
        # check if it's supported by the _locale module
        import _locale
        code, encoding = _locale._getdefaultlocale()
    except (ImportError, AttributeError):
        pass
    else:
        # make sure the code/encoding values are valid
        if sys.platform == "win32" and code and code[:2] == "0x":
            # map windows language identifier to language name
            code = windows_locale.get(int(code, 0))
        # ...add other platform-specific processing here, if
        # necessary...
        return code, encoding

    # fall back on POSIX behaviour
    import os
    lookup = os.environ.get
    for variable in envvars:
        localename = lookup(variable, None)
        if localename:
            if variable == 'LANGUAGE':
                localename = localename.split(':')[0]
            break
    else:
        localename = 'C'
    return _parse_localename(localename)


def getlocale(category=LC_CTYPE):
    """Return (language code, encoding) of the current setting of
    category (not LC_ALL)."""
    localename = _setlocale(category)
    if category == LC_ALL and ';' in localename:
        raise TypeError('category LC_ALL is not supported')
    return _parse_localename(localename)


def setlocale(category, locale=None):
    """Set the locale for category (locale may be a string, a
    (language, encoding) tuple or None to query)."""
    if locale and not isinstance(locale, _builtin_str):
        # convert to string
        locale = normalize(_build_localename(locale))
    return _setlocale(category, locale)


try:
    from _locale import getencoding
except ImportError:
    # When _locale.getencoding() is missing, locale.getencoding() uses the
    # Python filesystem encoding.
    def getencoding():
        return sys.getfilesystemencoding()


try:
    CODESET
except NameError:
    def getpreferredencoding(do_setlocale=True):
        """Return the charset that the user is likely using."""
        if sys.flags.warn_default_encoding:
            import warnings
            warnings.warn(
                "UTF-8 Mode affects locale.getpreferredencoding(). Consider locale.getencoding() instead.",
                EncodingWarning, 2)
        if sys.flags.utf8_mode:
            return 'utf-8'
        return getencoding()
else:
    # On Unix, if CODESET is available, use that.
    def getpreferredencoding(do_setlocale=True):
        """Return the charset that the user is likely using,
        according to the system configuration."""

        if sys.flags.warn_default_encoding:
            import warnings
            warnings.warn(
                "UTF-8 Mode affects locale.getpreferredencoding(). Consider locale.getencoding() instead.",
                EncodingWarning, 2)
        if sys.flags.utf8_mode:
            return 'utf-8'

        if not do_setlocale:
            return getencoding()

        old_loc = setlocale(LC_CTYPE)
        try:
            try:
                setlocale(LC_CTYPE, "")
            except Error:
                pass
            return getencoding()
        finally:
            setlocale(LC_CTYPE, old_loc)


### Database
#
# locale_encoding_alias maps normalized encoding names (lower case, no
# '-'/'_') to the encoding spelling used in C library locale names.
# locale_alias maps lookup names (lower case, encoding without '-'/'_')
# to full locale names.

locale_encoding_alias = {

    # Mappings for non-standard encoding names used in locale names
    '437':                          'C',
    'c':                            'C',
    'en':                           'ISO8859-1',
    'jis':                          'JIS7',
    'jis7':                         'JIS7',
    'ajec':                         'eucJP',
    'koi8c':                        'KOI8-C',
    'microsoftcp1251':              'CP1251',
    'microsoftcp1255':              'CP1255',
    'microsoftcp1256':              'CP1256',
    '88591':                        'ISO8859-1',
    '88592':                        'ISO8859-2',
    '88595':                        'ISO8859-5',
    '885915':                       'ISO8859-15',

    # Mappings from Python codec names to C lib encoding names
    'ascii':                        'ISO8859-1',
    'latin_1':                      'ISO8859-1',
    'iso8859_1':                    'ISO8859-1',
    'iso8859_10':                   'ISO8859-10',
    'iso8859_11':                   'ISO8859-11',
    'iso8859_13':                   'ISO8859-13',
    'iso8859_14':                   'ISO8859-14',
    'iso8859_15':                   'ISO8859-15',
    'iso8859_16':                   'ISO8859-16',
    'iso8859_2':                    'ISO8859-2',
    'iso8859_3':                    'ISO8859-3',
    'iso8859_4':                    'ISO8859-4',
    'iso8859_5':                    'ISO8859-5',
    'iso8859_6':                    'ISO8859-6',
    'iso8859_7':                    'ISO8859-7',
    'iso8859_8':                    'ISO8859-8',
    'iso8859_9':                    'ISO8859-9',
    'iso88599e':                    'ISO8859-9E',
    'iso2022_jp':                   'JIS7',
    'shift_jis':                    'SJIS',
    'tactis':                       'TACTIS',
    'euc_jp':                       'eucJP',
    'euc_kr':                       'eucKR',
    'utf_8':                        'UTF-8',
    'koi8_r':                       'KOI8-R',
    'koi8_t':                       'KOI8-T',
    'koi8_u':                       'KOI8-U',
    'kz1048':                       'RK1048',
    'cp1251':                       'CP1251',
    'cp1255':                       'CP1255',
    'cp1256':                       'CP1256',
    'georgianacademy':              'GEORGIAN-ACADEMY',
    'georgianps':                   'GEORGIAN-PS',
    'ibmcp1133':                    'IBM-CP1133',
    'cp1133':                       'IBM-CP1133',
}

# Language codes with their default locale; the variants below add
# common spellings of encodings and aliases.
_locale_defaults = {
    'af_za': 'ISO8859-1', 'am_et': 'UTF-8', 'an_es': 'ISO8859-15',
    'ar_aa': 'ISO8859-6', 'ar_ae': 'ISO8859-6', 'ar_bh': 'ISO8859-6',
    'ar_dz': 'ISO8859-6', 'ar_eg': 'ISO8859-6', 'ar_in': 'UTF-8',
    'ar_iq': 'ISO8859-6', 'ar_jo': 'ISO8859-6', 'ar_kw': 'ISO8859-6',
    'ar_lb': 'ISO8859-6', 'ar_ly': 'ISO8859-6', 'ar_ma': 'ISO8859-6',
    'ar_om': 'ISO8859-6', 'ar_qa': 'ISO8859-6', 'ar_sa': 'ISO8859-6',
    'ar_sd': 'ISO8859-6', 'ar_sy': 'ISO8859-6', 'ar_tn': 'ISO8859-6',
    'ar_ye': 'ISO8859-6', 'as_in': 'UTF-8', 'az_az': 'ISO8859-9E',
    'be_by': 'CP1251', 'bg_bg': 'CP1251', 'bn_bd': 'UTF-8', 'bn_in': 'UTF-8',
    'bo_cn': 'UTF-8', 'bo_in': 'UTF-8', 'br_fr': 'ISO8859-1',
    'bs_ba': 'ISO8859-2', 'ca_ad': 'ISO8859-1', 'ca_es': 'ISO8859-1',
    'ca_fr': 'ISO8859-1', 'ca_it': 'ISO8859-1', 'cs_cz': 'ISO8859-2',
    'cy_gb': 'ISO8859-1', 'da_dk': 'ISO8859-1', 'de_at': 'ISO8859-1',
    'de_be': 'ISO8859-1', 'de_ch': 'ISO8859-1', 'de_de': 'ISO8859-1',
    'de_it': 'ISO8859-1', 'de_li': 'UTF-8', 'de_lu': 'ISO8859-1',
    'el_cy': 'ISO8859-7', 'el_gr': 'ISO8859-7', 'en_ag': 'UTF-8',
    'en_au': 'ISO8859-1', 'en_be': 'ISO8859-1', 'en_bw': 'ISO8859-1',
    'en_ca': 'ISO8859-1', 'en_dk': 'ISO8859-1', 'en_gb': 'ISO8859-1',
    'en_hk': 'ISO8859-1', 'en_ie': 'ISO8859-1', 'en_il': 'UTF-8',
    'en_in': 'ISO8859-1', 'en_ng': 'UTF-8', 'en_nz': 'ISO8859-1',
    'en_ph': 'ISO8859-1', 'en_sc': 'UTF-8', 'en_sg': 'ISO8859-1',
    'en_uk': 'ISO8859-1', 'en_us': 'ISO8859-1', 'en_za': 'ISO8859-1',
    'en_zm': 'UTF-8', 'en_zw': 'ISO8859-1', 'eo_xx': 'ISO8859-3',
    'es_ar': 'ISO8859-1', 'es_bo': 'ISO8859-1', 'es_cl': 'ISO8859-1',
    'es_co': 'ISO8859-1', 'es_cr': 'ISO8859-1', 'es_cu': 'UTF-8',
    'es_do': 'ISO8859-1', 'es_ec': 'ISO8859-1', 'es_es': 'ISO8859-1',
    'es_gt': 'ISO8859-1', 'es_hn': 'ISO8859-1', 'es_mx': 'ISO8859-1',
    'es_ni': 'ISO8859-1', 'es_pa': 'ISO8859-1', 'es_pe': 'ISO8859-1',
    'es_pr': 'ISO8859-1', 'es_py': 'ISO8859-1', 'es_sv': 'ISO8859-1',
    'es_us': 'ISO8859-1', 'es_uy': 'ISO8859-1', 'es_ve': 'ISO8859-1',
    'et_ee': 'ISO8859-1', 'eu_es': 'ISO8859-1', 'eu_fr': 'ISO8859-1',
    'fa_ir': 'UTF-8', 'fi_fi': 'ISO8859-15', 'fo_fo': 'ISO8859-1',
    'fr_be': 'ISO8859-1', 'fr_ca': 'ISO8859-1', 'fr_ch': 'ISO8859-1',
    'fr_fr': 'ISO8859-1', 'fr_lu': 'ISO8859-1', 'fy_nl': 'UTF-8',
    'ga_ie': 'ISO8859-1', 'gd_gb': 'ISO8859-1', 'gl_es': 'ISO8859-1',
    'gu_in': 'UTF-8', 'gv_gb': 'ISO8859-1', 'he_il': 'ISO8859-8',
    'hi_in': 'ISCII-DEV', 'hr_hr': 'ISO8859-2', 'hu_hu': 'ISO8859-2',
    'hy_am': 'UTF-8', 'id_id': 'ISO8859-1', 'is_is': 'ISO8859-1',
    'it_ch': 'ISO8859-1', 'it_it': 'ISO8859-1', 'iw_il': 'ISO8859-8',
    'ja_jp': 'eucJP', 'ka_ge': 'GEORGIAN-ACADEMY', 'kk_kz': 'PTCP154',
    'kl_gl': 'ISO8859-1', 'km_kh': 'UTF-8', 'kn_in': 'UTF-8',
    'ko_kr': 'eucKR', 'ku_tr': 'ISO8859-9', 'kw_gb': 'ISO8859-1',
    'ky_kg': 'UTF-8', 'lo_la': 'MULELAO-1', 'lt_lt': 'ISO8859-13',
    'lv_lv': 'ISO8859-13', 'mi_nz': 'ISO8859-1', 'mk_mk': 'ISO8859-5',
    'ml_in': 'UTF-8', 'mn_mn': 'UTF-8', 'mr_in': 'UTF-8',
    'ms_my': 'ISO8859-1', 'mt_mt': 'ISO8859-3', 'my_mm': 'UTF-8',
    'nb_no': 'ISO8859-1', 'ne_np': 'UTF-8', 'nl_be': 'ISO8859-1',
    'nl_nl': 'ISO8859-1', 'nn_no': 'ISO8859-1', 'no_no': 'ISO8859-1',
    'oc_fr': 'ISO8859-1', 'or_in': 'UTF-8', 'pa_in': 'UTF-8',
    'pl_pl': 'ISO8859-2', 'ps_af': 'UTF-8', 'pt_br': 'ISO8859-1',
    'pt_pt': 'ISO8859-1', 'ro_ro': 'ISO8859-2', 'ru_ru': 'UTF-8',
    'ru_ua': 'KOI8-U', 'rw_rw': 'ISO8859-1', 'se_no': 'UTF-8',
    'si_lk': 'UTF-8', 'sk_sk': 'ISO8859-2', 'sl_si': 'ISO8859-2',
    'sq_al': 'ISO8859-2', 'sr_cs': 'UTF-8', 'sr_me': 'UTF-8',
    'sr_rs': 'UTF-8', 'sv_fi': 'ISO8859-1', 'sv_se': 'ISO8859-1',
    'sw_ke': 'UTF-8', 'ta_in': 'TSCII-0', 'te_in': 'UTF-8',
    'tg_tj': 'KOI8-C', 'th_th': 'ISO8859-11', 'tl_ph': 'ISO8859-1',
    'tr_tr': 'ISO8859-9', 'tt_ru': 'TATAR-CYR', 'ug_cn': 'UTF-8',
    'uk_ua': 'KOI8-U', 'ur_in': 'UTF-8', 'ur_pk': 'CP1256',
    'uz_uz': 'UTF-8', 've_za': 'UTF-8', 'vi_vn': 'TCVN',
    'wa_be': 'ISO8859-1', 'xh_za': 'ISO8859-1', 'yi_us': 'CP1255',
    'zh_cn': 'gb2312', 'zh_hk': 'big5hkscs', 'zh_sg': 'GB2312',
    'zh_tw': 'big5', 'zu_za': 'ISO8859-1', 'ks_in': 'UTF-8',
    'sd_in': 'UTF-8', 'shn_mm': 'UTF-8', 'lzh_tw': 'UTF-8',
}


def _make_locale_alias():
    alias = {}
    for code, enc in _locale_defaults.items():
        lang, terr = code.split('_')
        full = '%s_%s.%s' % (lang, terr.upper(), enc)
        alias[code] = full
        # Language alone maps to its first (alphabetical) territory unless
        # set explicitly below.
        alias.setdefault(lang, full)
    explicit = {
        'c': 'C',
        'c.ascii': 'C',
        'c.en': 'C',
        'c.iso88591': 'en_US.ISO8859-1',
        'c.utf8': 'C.UTF-8',
        'c_c': 'C',
        'c_c.c': 'C',
        'posix': 'C',
        'posix_iso_8859_1': 'en_US.ISO8859-1',
        'en': 'en_US.ISO8859-1',
        'en.iso88591': 'en_US.ISO8859-1',
        'en_us.utf8': 'en_US.UTF-8',
        'english': 'en_EN.ISO8859-1',
        'english_uk': 'en_GB.ISO8859-1',
        'english_united-states': 'en_US.ISO8859-1',
        'english_united-states.437': 'C',
        'english_us': 'en_US.ISO8859-1',
        'de': 'de_DE.ISO8859-1',
        'fr': 'fr_FR.ISO8859-1',
        'es': 'es_ES.ISO8859-1',
        'it': 'it_IT.ISO8859-1',
        'pt': 'pt_PT.ISO8859-1',
        'nl': 'nl_NL.ISO8859-1',
        'ru': 'ru_RU.UTF-8',
        'ca': 'ca_ES.ISO8859-1',
        'ar': 'ar_AA.ISO8859-6',
        'sv': 'sv_SE.ISO8859-1',
        'el': 'el_GR.ISO8859-7',
        'zh': 'zh_CN.eucCN',
        'zh_cn.euc': 'zh_CN.eucCN',
        'zh_tw.euc': 'zh_TW.eucTW',
        'zh_tw.euctw': 'zh_TW.eucTW',
        'ja': 'ja_JP.eucJP',
        'ja.jis': 'ja_JP.JIS7',
        'ja.sjis': 'ja_JP.SJIS',
        'ja_jp.ajec': 'ja_JP.eucJP',
        'ja_jp.euc': 'ja_JP.eucJP',
        'ja_jp.eucjp': 'ja_JP.eucJP',
        'ja_jp.iso2022jp': 'ja_JP.JIS7',
        'ja_jp.jis': 'ja_JP.JIS7',
        'ja_jp.jis7': 'ja_JP.JIS7',
        'ja_jp.mscode': 'ja_JP.SJIS',
        'ja_jp.pck': 'ja_JP.SJIS',
        'ja_jp.sjis': 'ja_JP.SJIS',
        'ja_jp.ujis': 'ja_JP.eucJP',
        'japan': 'ja_JP.eucJP',
        'japanese': 'ja_JP.eucJP',
        'japanese-euc': 'ja_JP.eucJP',
        'japanese.euc': 'ja_JP.eucJP',
        'japanese.sjis': 'ja_JP.SJIS',
        'jp_jp': 'ja_JP.eucJP',
        'ko': 'ko_KR.eucKR',
        'ko_kr.euc': 'ko_KR.eucKR',
        'ko_kr.euckr': 'ko_KR.eucKR',
        'korean': 'ko_KR.eucKR',
        'korean.euc': 'ko_KR.eucKR',
        'lo_la.cp1133': 'lo_LA.IBM-CP1133',
        'lo_la.ibmcp1133': 'lo_LA.IBM-CP1133',
        'uk_ua.microsoftcp1251': 'uk_UA.CP1251',
        'ka_ge.georgianacademy': 'ka_GE.GEORGIAN-ACADEMY',
        'ka_ge.georgianps': 'ka_GE.GEORGIAN-PS',
        'tt_ru.koi8c': 'tt_RU.KOI8-C',
        'tt_ru.tatarcyr': 'tt_RU.TATAR-CYR',
        'az_az.iso88599e': 'az_AZ.ISO8859-9E',
        'cs_cz.iso88592': 'cs_CZ.ISO8859-2',
        'de_de@euro': 'de_DE.ISO8859-15@euro',
        'en_us.iso885915@euro': 'en_US.ISO8859-15@euro',
        'be_by@latin': 'be_BY.UTF-8@latin',
        'sr_rs@latn': 'sr_RS.UTF-8@latin',
        'sr_rs@latin': 'sr_RS.UTF-8@latin',
        'ca_es@valencia': 'ca_ES.UTF-8@valencia',
        'ca@valencia': 'ca_ES.ISO8859-1@valencia',
        'ks': 'ks_IN.UTF-8',
        'ks_in@devanagari': 'ks_IN.UTF-8@devanagari',
        'ks@devanagari': 'ks_IN.UTF-8@devanagari',
        'sd': 'sd_IN.UTF-8',
        'sd_in@devanagari': 'sd_IN.UTF-8@devanagari',
        'sd@devanagari': 'sd_IN.UTF-8@devanagari',
        'ug_cn@latin': 'ug_CN.UTF-8@latin',
        'uz_uz@cyrillic': 'uz_UZ.UTF-8@cyrillic',
        'fr_fr@euro': 'fr_FR.ISO8859-15@euro',
        'ca_es@euro': 'ca_ES.ISO8859-15@euro',
        'el_gr@euro': 'el_GR.ISO8859-7@euro',
        'universal.utf8@ucs4': 'en_US.UTF-8',
    }
    alias.update(explicit)
    return alias


locale_alias = _make_locale_alias()

#
# Windows language identifiers (LCIDs) of the most common locales.
#
windows_locale = {
    0x0401: "ar_SA", 0x0402: "bg_BG", 0x0403: "ca_ES", 0x0404: "zh_TW",
    0x0405: "cs_CZ", 0x0406: "da_DK", 0x0407: "de_DE", 0x0408: "el_GR",
    0x0409: "en_US", 0x040a: "es_ES_tradnl", 0x040b: "fi_FI", 0x040c: "fr_FR",
    0x040d: "he_IL", 0x040e: "hu_HU", 0x040f: "is_IS", 0x0410: "it_IT",
    0x0411: "ja_JP", 0x0412: "ko_KR", 0x0413: "nl_NL", 0x0414: "nb_NO",
    0x0415: "pl_PL", 0x0416: "pt_BR", 0x0418: "ro_RO", 0x0419: "ru_RU",
    0x041a: "hr_HR", 0x041b: "sk_SK", 0x041c: "sq_AL", 0x041d: "sv_SE",
    0x041e: "th_TH", 0x041f: "tr_TR", 0x0420: "ur_PK", 0x0421: "id_ID",
    0x0422: "uk_UA", 0x0423: "be_BY", 0x0424: "sl_SI", 0x0425: "et_EE",
    0x0426: "lv_LV", 0x0427: "lt_LT", 0x0429: "fa_IR", 0x042a: "vi_VN",
    0x042b: "hy_AM", 0x042c: "az_AZ", 0x042d: "eu_ES", 0x042f: "mk_MK",
    0x0436: "af_ZA", 0x0437: "ka_GE", 0x0438: "fo_FO", 0x0439: "hi_IN",
    0x043e: "ms_MY", 0x043f: "kk_KZ", 0x0441: "sw_KE", 0x0443: "uz_UZ",
    0x0444: "tt_RU", 0x0445: "bn_IN", 0x0446: "pa_IN", 0x0447: "gu_IN",
    0x0449: "ta_IN", 0x044a: "te_IN", 0x044b: "kn_IN", 0x044e: "mr_IN",
    0x0456: "gl_ES", 0x0804: "zh_CN", 0x0807: "de_CH", 0x0809: "en_GB",
    0x080a: "es_MX", 0x080c: "fr_BE", 0x0810: "it_CH", 0x0813: "nl_BE",
    0x0814: "nn_NO", 0x0816: "pt_PT", 0x0c04: "zh_HK", 0x0c07: "de_AT",
    0x0c09: "en_AU", 0x0c0a: "es_ES", 0x0c0c: "fr_CA", 0x1004: "zh_SG",
    0x1009: "en_CA", 0x100c: "fr_CH", 0x1409: "en_NZ", 0x1809: "en_IE",
    0x1c09: "en_ZA",
}


def _print_locale():
    """Print the current locale settings."""
    categories = {}

    def _init_categories(categories=categories):
        for k, v in globals().items():
            if k[:3] == 'LC_':
                categories[k] = v
    _init_categories()
    del categories['LC_ALL']

    print('Locale defaults as determined by getdefaultlocale():')
    print('-' * 72)
    lang, enc = getdefaultlocale()
    print('Language: ', lang or '(undefined)')
    print('Encoding: ', enc or '(undefined)')
    print()

    print('Locale settings on startup:')
    print('-' * 72)
    for name, category in categories.items():
        print(name, '...')
        lang, enc = getlocale(category)
        print('   Language: ', lang or '(undefined)')
        print('   Encoding: ', enc or '(undefined)')
        print()


try:
    LC_MESSAGES
except NameError:
    pass
else:
    __all__.append("LC_MESSAGES")

if __name__ == '__main__':
    print('Locale aliasing:')
    print()
    _print_locale()
    print()
    print('Number formatting:')
    print()
    _test()
