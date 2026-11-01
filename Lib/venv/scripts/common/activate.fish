# Activation script for fish:
#
#   source <venv>/bin/activate.fish
#
# puts the environment's bin directory first on PATH and sets VIRTUAL_ENV.
# Run "deactivate" to undo.

function deactivate --description "Leave the virtual environment"
    if set -q _venv_saved_path
        set -gx PATH $_venv_saved_path
        set -e _venv_saved_path
    end
    if set -q _venv_saved_pythonhome
        set -gx PYTHONHOME $_venv_saved_pythonhome
        set -e _venv_saved_pythonhome
    end
    if functions -q _venv_saved_fish_prompt
        functions -e fish_prompt
        functions -c _venv_saved_fish_prompt fish_prompt
        functions -e _venv_saved_fish_prompt
    end
    set -e VIRTUAL_ENV
    set -e VIRTUAL_ENV_PROMPT
    if test "$argv[1]" != nondestructive
        functions -e deactivate
        functions -e _venv_return
    end
end

# Leave any environment that is currently active.
deactivate nondestructive

set -gx VIRTUAL_ENV __VENV_DIR__
set -gx _venv_saved_path $PATH
set -gx PATH "$VIRTUAL_ENV/"__VENV_BIN_NAME__ $PATH
set -gx VIRTUAL_ENV_PROMPT __VENV_PROMPT__

if set -q PYTHONHOME
    set -gx _venv_saved_pythonhome $PYTHONHOME
    set -e PYTHONHOME
end

# Lets the wrapped prompt see the exit status of the last command.
function _venv_return
    return $argv[1]
end

if test -z "$VIRTUAL_ENV_DISABLE_PROMPT"
    functions -c fish_prompt _venv_saved_fish_prompt
    function fish_prompt
        set -l last_status $status
        printf '(%s) ' $VIRTUAL_ENV_PROMPT
        _venv_return $last_status
        _venv_saved_fish_prompt
    end
end
