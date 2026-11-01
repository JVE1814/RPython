# Activation script for csh/tcsh:
#
#   source <venv>/bin/activate.csh
#
# puts the environment's bin directory first on PATH and sets VIRTUAL_ENV.
# Run "deactivate" to undo.

alias deactivate 'if ($?_venv_saved_path != 0) setenv PATH "$_venv_saved_path:q"; unset _venv_saved_path; if ($?_venv_saved_prompt != 0) set prompt="$_venv_saved_prompt:q"; unset _venv_saved_prompt; unsetenv VIRTUAL_ENV; unsetenv VIRTUAL_ENV_PROMPT; rehash; if ("\!:*" != "nondestructive") unalias deactivate'

# Leave any environment that is currently active.
deactivate nondestructive

setenv VIRTUAL_ENV __VENV_DIR__
set _venv_bin = __VENV_BIN_NAME__
set _venv_saved_path = "$PATH:q"
setenv PATH "${VIRTUAL_ENV:q}/${_venv_bin:q}:${PATH:q}"
unset _venv_bin
setenv VIRTUAL_ENV_PROMPT __VENV_PROMPT__

if (! $?VIRTUAL_ENV_DISABLE_PROMPT) then
    if ($?prompt) then
        set _venv_saved_prompt = "$prompt:q"
        set prompt = "(${VIRTUAL_ENV_PROMPT:q}) $prompt:q"
    endif
endif

rehash
