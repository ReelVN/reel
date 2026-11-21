//! Register an extracted Linux release in the current user's application menu.
use super::Identity;
use image::{DynamicImage, ImageFormat};
use std::{fs::File, io, io::Write, path::Path};

pub(super) fn write_menu(
    root: &Path,
    identity: &Identity,
    icon: Option<DynamicImage>,
) -> io::Result<u64> {
    let has_icon = icon.is_some();
    let mut bytes = 0;
    if let Some(icon) = icon {
        let mut file = File::create_new(root.join("game-icon.png"))?;
        icon.write_to(&mut file, ImageFormat::Png)
            .map_err(io::Error::other)?;
        bytes += file.metadata()?.len();
    }

    // Escape the desktop entry value before quoting it as a shell literal.
    let name = identity
        .title
        .replace('\\', r"\\")
        .replace(' ', r"\s")
        .replace('\'', r"'\''");
    let script = format!(
        "#!/bin/sh\nset -eu\n\nid='{}'\nname='{name}'\nhas_icon={has_icon}\n{REGISTER_MENU}",
        identity.id
    );
    File::create_new(root.join("install-menu.sh"))?.write_all(script.as_bytes())?;
    Ok(bytes + script.len() as u64)
}

const REGISTER_MENU: &str = r#"
fail() {
    printf '%s\n' "$1" >&2
    exit 1
}

case $0 in
    /*) script=$0 ;;
    *) script=$PWD/$0 ;;
esac
release=$(CDPATH= cd -P "${script%/*}/" && printf '%s/' "${PWD%/}")
game=${release}game
if [ ! -f "$game" ] || [ ! -x "$game" ]; then
    fail 'Expected an executable game beside install-menu.sh. Extract the full release and run chmod +x game if needed.'
fi
if [ ! -f "${release}game.asb" ]; then
    fail 'Expected game.asb beside install-menu.sh. Extract the full release before registering it.'
fi
# Exec is parsed first as a desktop entry value and then as a quoted argument.
desktop_path() {
    REEL_DESKTOP_PATH=$1 REEL_EXEC_FIELD=$2 LC_ALL=C awk '
BEGIN {
    path = ENVIRON["REEL_DESKTOP_PATH"]
    exec_field = ENVIRON["REEL_EXEC_FIELD"] == "true"
    for (i = 1; i <= length(path); i++) {
        ch = substr(path, i, 1)
        if (ch == "\\") {
            printf "%s", exec_field ? "\\\\\\\\" : "\\\\"
        } else if (ch == "\n") {
            printf "\\n"
        } else if (ch == "\r") {
            printf "\\r"
        } else if (ch == "\t") {
            printf "\\t"
        } else if (exec_field && (ch == "\"" || ch == "$" || ch == "`")) {
            printf "\\\\%s", ch
        } else if (exec_field && ch == "%") {
            printf "%%%%"
        } else {
            printf "%s", ch
        }
    }
}'
}

executable=$(desktop_path "$game" true)
case $game in
    # GIO checks the executable before resolving %%, and the spec forbids equals
    # signs in the executable token. Pass such paths as a separate argument.
    *%*|*=*) executable='/bin/sh -c "exec \\"\\$1\\"" sh '"\"$executable\"" ;;
    *) executable="\"$executable\"" ;;
esac
directory=$(desktop_path "$release" false)
if [ "$has_icon" = true ]; then
    icon=${release}game-icon.png
    if [ ! -f "$icon" ]; then
        fail 'Expected game-icon.png beside install-menu.sh. Extract the full release before registering it.'
    fi
    icon=$(desktop_path "$icon" false)
fi

case ${XDG_DATA_HOME:-} in
    /*) data_home=$XDG_DATA_HOME ;;
    *)
        case ${HOME:-} in
            /*) data_home=${HOME%/}/.local/share ;;
            *) fail 'Set XDG_DATA_HOME or HOME to an absolute path.' ;;
        esac
        ;;
esac
applications=${data_home%/}/applications
umask 077
mkdir -p "$applications"
entry=$applications/$id.desktop
temporary=$(mktemp "$applications/.$id.XXXXXX")
trap 'rm -f "$temporary"' 0
{
    printf '[Desktop Entry]\nType=Application\nName=%s\nExec=%s\nPath=%s\n' "$name" "$executable" "$directory"
    if [ "$has_icon" = true ]; then
        printf 'Icon=%s\n' "$icon"
    fi
    printf 'Terminal=false\nCategories=Game;\nStartupWMClass=%s\n' "$id"
} > "$temporary"
mv -fT "$temporary" "$entry"
printf 'Registered %s\nRun this script again if you move the game.\n' "$entry"
"#;
