# ANSI color helpers — source this in any dip command:
#   source "${DIP_DIR}/commands/utils/color.sh"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NOFORMAT='\033[0m'

msg() {
  echo -e "$*"
}
