//! # Client
//!
//! module which exposes the Smb Client

use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{mem, ptr};

use libc::{self, c_char, c_int};
use pavao_sys::{SMBCCTX, *};

use super::{
    AuthService, SmbCredentials, SmbDirentInfo, SmbFile, SmbMode, SmbOpenOptions, SmbOptions,
    SmbStat, SmbStatVfs,
};
use crate::{utils, SmbDirent, SmbError, SmbResult};

struct SmbContext {
    inner: *mut SMBCCTX,
}

impl SmbContext {
    /// Create a null context
    fn null() -> Self {
        SmbContext {
            inner: ptr::null_mut(),
        }
    }

    /// Set context
    pub fn set(&mut self, ctx: *mut SMBCCTX) {
        self.inner = ctx;
    }

    /// Get context
    pub fn get(&self) -> *mut SMBCCTX {
        self.inner
    }

    /// Check if context is null
    pub fn is_null(&self) -> bool {
        self.get().is_null()
    }
}

unsafe impl Sync for SmbContext {}
unsafe impl Send for SmbContext {}

lazy_static! {
    static ref AUTH_SERVICE: Mutex<AuthService> = Mutex::new(AuthService::default());
    static ref SMBCTX: Arc<Mutex<SmbContext>> = Arc::new(Mutex::new(SmbContext::null()));
}

/// Smb protocol client
pub struct SmbClient {
    uri: String,
}

impl SmbClient {
    /// Initialize a new `SmbClient` with the provided credentials to connect to the remote smb server
    pub fn new(credentials: SmbCredentials, options: SmbOptions) -> SmbResult<Self> {
        let uri = Self::build_uri(credentials.server.as_str(), credentials.share.as_str());
        let smbc = SmbClient { uri };
        // insert credentials
        trace!("creating context...");
        // get current context
        let mut ctx_lock = SMBCTX.lock().map_err(|_| SmbError::Mutex)?;
        // if context is null, create a new one
        if ctx_lock.is_null() {
            unsafe {
                let ctx = utils::result_from_ptr_mut(smbc_new_context())?;
                // set options
                trace!("configuring client options");
                smbc_setFunctionAuthDataWithContext(ctx, Some(Self::auth_wrapper));
                Self::setup_options(ctx, options);

                // set ctx
                let smb_ctx = utils::result_from_ptr_mut(smbc_init_context(ctx))?;
                trace!("context initialized");
                AUTH_SERVICE
                    .lock()
                    .unwrap()
                    .insert(Self::auth_service_uuid(smb_ctx), credentials);

                // set context
                ctx_lock.set(smb_ctx);
            }
        }
        Ok(smbc)
    }

    /// Get netbios name from server
    pub fn get_netbios_name(&self) -> SmbResult<String> {
        trace!("getting netbios name");
        unsafe {
            let ptr = utils::result_from_ptr_mut(smbc_getNetbiosName(self.ctx()?))?;
            utils::char_ptr_to_string(ptr).map_err(|_| SmbError::BadValue)
        }
    }

    /// Set netbios name to server
    pub fn set_netbios_name<S>(&self, name: S) -> SmbResult<()>
    where
        S: AsRef<str>,
    {
        trace!("setting netbios name to {}", name.as_ref());
        let name = utils::str_to_cstring(name)?;
        unsafe { smbc_setNetbiosName(self.ctx()?, name.into_raw()) }
        Ok(())
    }

    /// Get workgroup name from server
    pub fn get_workgroup(&self) -> SmbResult<String> {
        trace!("getting workgroup");
        unsafe {
            let ptr = utils::result_from_ptr_mut(smbc_getWorkgroup(self.ctx()?))?;
            utils::char_ptr_to_string(ptr).map_err(|_| SmbError::BadValue)
        }
    }

    /// Set workgroup name to server
    pub fn set_workgroup<S>(&self, name: S) -> SmbResult<()>
    where
        S: AsRef<str>,
    {
        trace!("configuring workgroup to {}", name.as_ref());
        let name = utils::str_to_cstring(name)?;
        unsafe { smbc_setWorkgroup(self.ctx()?, name.into_raw()) }
        Ok(())
    }

    /// Get get_user name from server
    pub fn get_user(&self) -> SmbResult<String> {
        trace!("getting current username");
        unsafe {
            let ptr = utils::result_from_ptr_mut(smbc_getUser(self.ctx()?))?;
            utils::char_ptr_to_string(ptr).map_err(|_| SmbError::BadValue)
        }
    }

    /// Set user name to server
    pub fn set_user<S>(&self, name: S) -> SmbResult<()>
    where
        S: AsRef<str>,
    {
        trace!("configuring current username as {}", name.as_ref());
        let name = utils::str_to_cstring(name)?;
        unsafe { smbc_setUser(self.ctx()?, name.into_raw()) }
        Ok(())
    }

    /// Get timeout from server
    pub fn get_timeout(&self) -> SmbResult<Duration> {
        trace!("getting timeout");
        unsafe { Ok(Duration::from_millis(smbc_getTimeout(self.ctx()?) as u64)) }
    }

    /// Set timeout to server
    pub fn set_timeout(&self, timeout: Duration) -> SmbResult<()> {
        trace!("setting timeout to {}ms", timeout.as_millis());
        unsafe { smbc_setTimeout(self.ctx()?, timeout.as_millis() as c_int) }
        Ok(())
    }

    /// Get smbc version
    pub fn get_version(&self) -> SmbResult<String> {
        trace!("getting smb version");
        unsafe {
            let ptr = smbc_version();
            utils::char_ptr_to_string(ptr).map_err(|_| SmbError::BadValue)
        }
    }

    /// Unlink file at `path`
    pub fn unlink<S>(&self, path: S) -> SmbResult<()>
    where
        S: AsRef<str>,
    {
        trace!("unlinking entry at {}", path.as_ref());
        let path = utils::str_to_cstring(self.uri(path))?;
        let unlink_fn = self.get_fn(self.ctx()?, smbc_getFunctionUnlink)?;
        utils::to_result_with_ioerror((), unlink_fn(self.ctx()?, path.as_ptr()))
    }

    /// Rename file at `orig_url` to `new_url`
    pub fn rename<S>(&self, orig_url: S, new_url: S) -> SmbResult<()>
    where
        S: AsRef<str>,
    {
        trace!("renaming {} to {}", orig_url.as_ref(), new_url.as_ref());
        let orig_url = utils::str_to_cstring(self.uri(orig_url))?;
        let new_url = utils::str_to_cstring(self.uri(new_url))?;
        let rename_fn = self.get_fn(self.ctx()?, smbc_getFunctionRename)?;
        utils::to_result_with_ioerror(
            (),
            rename_fn(
                self.ctx()?,
                orig_url.as_ptr(),
                self.ctx()?,
                new_url.as_ptr(),
            ),
        )
    }

    /// List content of directory at `path`
    pub fn list_dir<S>(&self, path: S) -> SmbResult<Vec<SmbDirent>>
    where
        S: AsRef<str>,
    {
        trace!("listing files at {}", path.as_ref());
        let path = utils::str_to_cstring(self.uri(path))?;
        let opendir_fn = self.get_fn(self.ctx()?, smbc_getFunctionOpendir)?;
        let fd = opendir_fn(self.ctx()?, path.as_ptr());
        if fd.is_null() {
            error!("failed to open directory: returned a bad file descriptor");
            return Err(SmbError::BadFileDescriptor);
        }
        let closedir_fn = self.get_fn(self.ctx()?, smbc_getFunctionClosedir)?;
        let mut entries = Vec::new();
        let readdir_fn = self.get_fn(self.ctx()?, smbc_getFunctionReaddir)?;
        loop {
            let dirent = readdir_fn(self.ctx()?, fd);
            if dirent.is_null() {
                break;
            }
            unsafe {
                match SmbDirent::try_from(*dirent) {
                    Ok(dirent)
                        if dirent.name() != "."
                            && dirent.name() != ".."
                            && !dirent.name().is_empty() =>
                    {
                        trace!("found dirent: {:?}", dirent);
                        entries.push(dirent);
                    }
                    Ok(_) => {
                        trace!("ignoring '..', '.' directories");
                    }
                    Err(e) => {
                        error!("failed to decode directory entity {:?}: {}", dirent, e);
                    }
                }
            }
        }
        trace!("decoded {} dirents", entries.len());
        // Close directory
        let _ = closedir_fn(self.ctx()?, fd);
        Ok(entries)
    }

    /// List content of directory with metadata at 'path'
    pub fn list_dirplus<S>(&self, path: S) -> SmbResult<Vec<SmbDirentInfo>>
    where
        S: AsRef<str>,
    {
        trace!("listing files with metadata at {}", path.as_ref());
        let path = utils::str_to_cstring(self.uri(path))?;
        let opendir_fn = self.get_fn(self.ctx()?, smbc_getFunctionOpendir)?;
        let fd = opendir_fn(self.ctx()?, path.as_ptr());
        if fd.is_null() {
            error!("failed to open directory: returned a bad file descriptor");
            return Err(SmbError::BadFileDescriptor);
        }
        let closedir_fn = self.get_fn(self.ctx()?, smbc_getFunctionClosedir)?;
        let mut entries = Vec::new();
        let readdirplus_fn = self.get_fn(self.ctx()?, smbc_getFunctionReaddirPlus)?;
        loop {
            let direntplus = readdirplus_fn(self.ctx()?, fd);
            if direntplus.is_null() {
                break;
            }
            unsafe {
                match SmbDirentInfo::try_from(*direntplus) {
                    Ok(direntplus)
                        if direntplus.name() != "."
                            && direntplus.name() != ".."
                            && !direntplus.name().is_empty() =>
                    {
                        trace!("found direntplus: {:?}", direntplus);
                        entries.push(direntplus);
                    }
                    Ok(_) => {
                        trace!("ignoring '..', '.' directories");
                    }
                    Err(e) => {
                        error!(
                            "failed to decode directory entity with metadata {:?}: {}",
                            direntplus, e
                        );
                    }
                }
            }
        }
        trace!("decoded {} direntpluses", entries.len());
        // Close directory
        let _ = closedir_fn(self.ctx()?, fd);
        Ok(entries)
    }

    /// Make directory at `p` with provided `mode`
    pub fn mkdir<S>(&self, p: S, mode: SmbMode) -> SmbResult<()>
    where
        S: AsRef<str>,
    {
        trace!("making directory at {} with mode {:?}", p.as_ref(), mode);
        let p = utils::str_to_cstring(self.uri(p))?;
        let mkdir_fn = self.get_fn(self.ctx()?, smbc_getFunctionMkdir)?;
        utils::to_result_with_ioerror((), mkdir_fn(self.ctx()?, p.as_ptr(), mode.into()))
    }

    /// Remove directory at `p`
    pub fn rmdir<S>(&self, p: S) -> SmbResult<()>
    where
        S: AsRef<str>,
    {
        trace!("removing directory at {}", p.as_ref());
        let p = utils::str_to_cstring(self.uri(p))?;
        let rmdir_fn = self.get_fn(self.ctx()?, smbc_getFunctionRmdir)?;
        utils::to_result_with_ioerror((), rmdir_fn(self.ctx()?, p.as_ptr()))
    }

    /// Stat filesystem at `p` and return its metadata
    pub fn statvfs<S>(&self, p: S) -> SmbResult<SmbStatVfs>
    where
        S: AsRef<str>,
    {
        trace!("Stating filesystem at {}", p.as_ref());
        let p = utils::str_to_cstring(self.uri(p))?;
        unsafe {
            let mut st: libc::statvfs = mem::zeroed();
            let statvfs_fn = self.get_fn(self.ctx()?, smbc_getFunctionStatVFS)?;
            if statvfs_fn(self.ctx()?, p.as_ptr(), &mut st) < 0 {
                error!("failed to stat filesystem: {}", utils::last_os_error());
                Err(utils::last_os_error())
            } else {
                Ok(SmbStatVfs::from(st))
            }
        }
    }

    /// Stat file at `p` and return its metadata
    pub fn stat<S>(&self, p: S) -> SmbResult<SmbStat>
    where
        S: AsRef<str>,
    {
        trace!("Stating file at {}", p.as_ref());
        let p = utils::str_to_cstring(self.uri(p))?;
        unsafe {
            let mut st: libc::stat = mem::zeroed();
            let stat_fn = self.get_fn(self.ctx()?, smbc_getFunctionStat)?;
            if stat_fn(self.ctx()?, p.as_ptr(), &mut st) < 0 {
                error!("failed to stat file: {}", utils::last_os_error());
                Err(utils::last_os_error())
            } else {
                Ok(SmbStat::from(st))
            }
        }
    }

    /// Change file mode for file at `p`
    pub fn chmod<S>(&self, p: S, mode: SmbMode) -> SmbResult<()>
    where
        S: AsRef<str>,
    {
        trace!("changing mode for {} with {:?}", p.as_ref(), mode);
        let p = utils::str_to_cstring(self.uri(p))?;
        let chmod_fn = self.get_fn(self.ctx()?, smbc_getFunctionChmod)?;
        utils::to_result_with_ioerror((), chmod_fn(self.ctx()?, p.as_ptr(), mode.into()))
    }

    /// Print file at `p` using the `print_queue`
    pub fn print<S>(&self, p: S, print_queue: S) -> SmbResult<()>
    where
        S: AsRef<str>,
    {
        trace!("printing {} to {} queue", p.as_ref(), print_queue.as_ref());
        let p = utils::str_to_cstring(self.uri(p))?;
        let print_queue = utils::str_to_cstring(self.uri(print_queue))?;
        let print_fn = self.get_fn(self.ctx()?, smbc_getFunctionPrintFile)?;
        utils::to_result_with_ioerror(
            (),
            print_fn(self.ctx()?, p.as_ptr(), self.ctx()?, print_queue.as_ptr()),
        )
    }

    // -- internal private

    /// Build connection uri
    fn build_uri(server: &str, share: &str) -> String {
        format!(
            "{}{}{}",
            server,
            match share.starts_with('/') {
                true => "",
                false => "/",
            },
            share
        )
    }

    /// Get file uri
    fn uri<S>(&self, p: S) -> String
    where
        S: AsRef<str>,
    {
        format!("{}{}", self.uri, p.as_ref())
    }

    /// Callback getter
    #[allow(improper_ctypes_definitions)]
    pub(crate) fn get_fn<T>(
        &self,
        ctx: *mut SMBCCTX,
        get_func: unsafe extern "C" fn(*mut SMBCCTX) -> Option<T>,
    ) -> std::io::Result<T> {
        unsafe { get_func(ctx).ok_or_else(|| std::io::Error::from_raw_os_error(libc::EINVAL)) }
    }

    /// Setup options in the context
    unsafe fn setup_options(ctx: *mut SMBCCTX, options: SmbOptions) {
        smbc_setOptionBrowseMaxLmbCount(ctx, options.browser_max_lmb_count);
        smbc_setOptionCaseSensitive(ctx, options.case_sensitive as i32);
        smbc_setOptionDebugToStderr(ctx, 0);
        smbc_setOptionFallbackAfterKerberos(ctx, options.fallback_after_kerberos as i32);
        smbc_setOptionNoAutoAnonymousLogin(ctx, options.no_auto_anonymous_login as i32);
        smbc_setOptionOneSharePerServer(ctx, options.one_share_per_server as i32);
        smbc_setOptionOpenShareMode(ctx, options.open_share_mode.into());
        smbc_setOptionSmbEncryptionLevel(ctx, options.encryption_level.into());
        smbc_setOptionUrlEncodeReaddirEntries(ctx, options.url_encode_readdir_entries as i32);
        smbc_setOptionUseCCache(ctx, options.use_ccache as i32);
        smbc_setOptionUseKerberos(ctx, options.use_kerberos as i32);
        #[cfg(feature = "debug")]
        smbc_setOptionDebugToStderr(ctx, 1 as i32);
        #[cfg(feature = "debug")]
        smbc_setDebug(ctx, 10);
    }

    /// Auth wrapper passed to `SMBCCTX` to authenticate requests to SMB servers.
    extern "C" fn auth_wrapper(
        ctx: *mut SMBCCTX,
        srv: *const c_char,
        shr: *const c_char,
        wg: *mut c_char,
        wglen: c_int,
        un: *mut c_char,
        unlen: c_int,
        pw: *mut c_char,
        pwlen: c_int,
    ) {
        unsafe {
            let srv = utils::cstr(srv);
            let shr = utils::cstr(shr);
            trace!("authenticating on {}\\{}", &srv, &shr);
            let creds = AUTH_SERVICE
                .lock()
                .unwrap()
                .get(Self::auth_service_uuid(ctx))
                .clone();
            utils::write_to_cstr(wg as *mut u8, wglen as usize, &creds.workgroup);
            utils::write_to_cstr(un as *mut u8, unlen as usize, &creds.username);
            utils::write_to_cstr(pw as *mut u8, pwlen as usize, &creds.password);
        }
    }

    fn auth_service_uuid(ctx: *mut SMBCCTX) -> String {
        format!("{:?}", ctx)
    }

    /// Get underlying context
    pub fn ctx(&self) -> SmbResult<*mut SMBCCTX> {
        Ok(SMBCTX.lock().map_err(|_| SmbError::Mutex)?.get())
    }
}

impl<'a> SmbClient {
    /// Open a file at `P` with provided options
    pub fn open_with<P: AsRef<str>>(
        &'a self,
        path: P,
        options: SmbOpenOptions,
    ) -> SmbResult<SmbFile<'a>> {
        trace!("opening {} with {:?}", path.as_ref(), options);
        let open_fn = self.get_fn(self.ctx()?, smbc_getFunctionOpen)?;
        let path = utils::str_to_cstring(self.uri(path))?;
        let fd = utils::result_from_ptr_mut(open_fn(
            self.ctx()?,
            path.as_ptr(),
            options.to_flags(),
            options.mode,
        ))?;
        if (fd as i64) < 0 {
            error!("got a negative file descriptor");
            Err(SmbError::BadFileDescriptor)
        } else {
            trace!("opened file with file descriptor {:?}", fd);
            Ok(SmbFile::new(self, fd))
        }
    }
}

// -- destructor
impl Drop for SmbClient {
    fn drop(&mut self) {
        trace!("removing uri from auth service");
        unsafe {
            if let Ok(mut context) = SMBCTX.lock() {
                AUTH_SERVICE
                    .lock()
                    .unwrap()
                    .remove(Self::auth_service_uuid(context.get()));
                trace!("closing smbclient");
                smbc_free_context(context.get(), 1_i32);
                // set context to null
                context.set(ptr::null_mut());
            }
        }
        trace!("smbclient context freed");
    }
}

#[cfg(test)]
mod test {
    use std::io::{Cursor, Read};
    use std::time::UNIX_EPOCH;

    use pretty_assertions::{assert_eq, assert_ne};
    use serial_test::serial;

    use super::*;
    use crate::test::TestCtx;
    use crate::{mock, SmbDirentType};

    #[test]
    #[serial]
    fn should_initialize_client() {
        mock::logger();
        let ctx = init_ctx();
        assert_eq!(ctx.client.ctx().unwrap().is_null(), false);
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_get_netbios() {
        mock::logger();
        let ctx = init_ctx();
        assert!(ctx.client.get_netbios_name().is_ok());
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_set_netbios() {
        mock::logger();
        let ctx = init_ctx();
        assert!(ctx.client.set_netbios_name("foobar").is_ok());
        assert_eq!(ctx.client.get_netbios_name().unwrap().as_str(), "foobar");
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_get_workgroup() {
        mock::logger();
        let ctx = init_ctx();
        assert!(ctx.client.get_workgroup().is_ok());
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_set_workgroup() {
        mock::logger();
        let ctx = init_ctx();
        assert!(ctx.client.set_workgroup("foobar").is_ok());
        assert_eq!(ctx.client.get_workgroup().unwrap().as_str(), "foobar");
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_get_user() {
        mock::logger();
        let ctx = init_ctx();
        assert!(ctx.client.get_user().is_ok());
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_set_user() {
        mock::logger();
        let ctx = init_ctx();
        assert!(ctx.client.set_user("test").is_ok());
        assert_eq!(ctx.client.get_user().unwrap().as_str(), "test");
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_get_timeout() {
        mock::logger();
        let ctx = init_ctx();
        assert!(ctx.client.get_timeout().is_ok());
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_set_timeout() {
        mock::logger();
        let ctx = init_ctx();
        assert!(ctx.client.set_timeout(Duration::from_secs(3)).is_ok());
        assert_eq!(ctx.client.get_timeout().unwrap(), Duration::from_secs(3));
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_get_version() {
        mock::logger();
        let ctx = init_ctx();
        assert!(ctx.client.get_version().is_ok());
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_unlink() {
        mock::logger();
        let ctx = init_ctx();
        create_file_at(&ctx.client, "/cargo-test/test", "Hello, World!\n");
        let _ = ctx.client.unlink("/cargo-test/test"); // NOTE: may not be supported by the server
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_rename() {
        mock::logger();
        let ctx = init_ctx();
        create_file_at(&ctx.client, "/cargo-test/test", "Hello, World!\n");
        let _ = ctx.client.rename("/cargo-test/test", "/cargo-test/new"); // NOTE: may not be supported by the server
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_list_dir() {
        mock::logger();
        let ctx = init_ctx();
        create_file_at(&ctx.client, "/cargo-test/abc", "Hello, World!\n");
        create_file_at(&ctx.client, "/cargo-test/def", "Hello, World!\n");
        assert!(ctx
            .client
            .mkdir("/cargo-test/jfk", SmbMode::from(0o755))
            .is_ok());
        // list dir
        let mut entries = ctx.client.list_dir("/cargo-test").unwrap();
        entries.sort_by(|a, b| a.name().cmp(&b.name()));
        assert_eq!(entries.len(), 3);
        let abc = entries.get(0).unwrap();
        assert_eq!(abc.name(), "abc");
        assert_eq!(abc.get_type(), SmbDirentType::File);
        let def = entries.get(1).unwrap();
        assert_eq!(def.name(), "def");
        assert_eq!(def.get_type(), SmbDirentType::File);
        let jfk = entries.get(2).unwrap();
        assert_eq!(jfk.name(), "jfk");
        assert_eq!(jfk.get_type(), SmbDirentType::Dir);
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_list_dirplus() {
        mock::logger();
        let ctx = init_ctx();
        create_file_at(&ctx.client, "/cargo-test/ghi", "Hello, World!\n");
        create_file_at(&ctx.client, "/cargo-test/jkl", "Hello, World!\n");
        assert!(ctx
            .client
            .mkdir("/cargo-test/hil", SmbMode::from(0o755))
            .is_ok());
        // list dir
        let mut entries = ctx.client.list_dir("/cargo-test").unwrap();
        entries.sort_by(|a, b| a.name().cmp(&b.name()));
        assert_eq!(entries.len(), 3);
        let abc = entries.get(0).unwrap();
        assert_eq!(abc.name(), "ghi");
        assert_eq!(abc.get_type(), SmbDirentType::File);
        let def = entries.get(1).unwrap();
        assert_eq!(def.name(), "hil");
        assert_eq!(def.get_type(), SmbDirentType::Dir);
        let jfk = entries.get(2).unwrap();
        assert_eq!(jfk.name(), "jkl");
        assert_eq!(jfk.get_type(), SmbDirentType::File);
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_mkdir() {
        mock::logger();
        let ctx = init_ctx();
        assert!(ctx
            .client
            .mkdir("/cargo-test/testdir", SmbMode::from(0o755))
            .is_ok());
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_rmdir() {
        mock::logger();
        let ctx = init_ctx();
        assert!(ctx
            .client
            .mkdir("/cargo-test/testdir", SmbMode::from(0o755))
            .is_ok());
        // will return err on this server
        let _ = ctx.client.rmdir("/cargo-test/testdir");
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_statvfs() {
        mock::logger();
        let ctx = init_ctx();
        assert!(ctx.client.statvfs("/cargo-test").is_ok());
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_stat() {
        mock::logger();
        let ctx = init_ctx();
        // Create file
        create_file_at(&ctx.client, "/cargo-test/test", "Hello, World!\n");
        // stat
        let file = ctx.client.stat("/cargo-test/test").unwrap();
        assert_ne!(file.accessed, UNIX_EPOCH);
        assert_ne!(file.blksize, 0);
        assert_ne!(file.blocks, 0);
        //assert_eq!(file.mode, SmbMode::from(0o744));
        assert_eq!(file.size, 14);
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_chmod() {
        mock::logger();
        let ctx = init_ctx();
        // Create file
        create_file_at(&ctx.client, "/cargo-test/test", "Hello, World!\n");
        let _ = ctx.client.chmod("/cargo-test/test", SmbMode::from(0o755)); // NOTE: may not be supported by the server
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_build_uri() {
        mock::logger();
        let ctx = init_ctx();

        assert!(ctx.client.uri("/test").as_str().ends_with("/temp/test"));
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_read_file() {
        mock::logger();
        let ctx = init_ctx();
        create_file_at(&ctx.client, "/cargo-test/test", "Hello, World!\n");
        // read file
        let mut reader = ctx
            .client
            .open_with("/cargo-test/test", SmbOpenOptions::default().read(true))
            .unwrap();
        let mut output = String::default();
        assert!(reader.read_to_string(&mut output).is_ok());
        drop(reader);
        assert_eq!(output.as_str(), "Hello, World!\n");
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_write_file() {
        mock::logger();
        let ctx = init_ctx();
        // write file
        let mut writer = ctx
            .client
            .open_with(
                "/cargo-test/test",
                SmbOpenOptions::default().write(true).create(true),
            )
            .unwrap();
        let mut reader = Cursor::new("test string\n".as_bytes());
        assert_eq!(std::io::copy(&mut reader, &mut writer).unwrap(), 12);
        drop(writer);
        finalize_ctx(ctx);
    }

    #[test]
    #[serial]
    fn should_append_to_file() {
        mock::logger();
        let ctx = init_ctx();
        create_file_at(&ctx.client, "/cargo-test/test", "Hello, World!\n");
        // append
        let mut writer = ctx
            .client
            .open_with(
                "/cargo-test/test",
                SmbOpenOptions::default().write(true).append(true),
            )
            .unwrap();
        let mut reader = Cursor::new("Bonjour\n".as_bytes());
        assert_eq!(std::io::copy(&mut reader, &mut writer).unwrap(), 8);
        drop(writer);
        // read
        let mut reader = ctx
            .client
            .open_with("/cargo-test/test", SmbOpenOptions::default().read(true))
            .unwrap();
        let mut output = String::default();
        assert!(reader.read_to_string(&mut output).is_ok());
        drop(reader);
        assert_eq!(output.as_str(), "Hello, World!\nBonjour\n");
        finalize_ctx(ctx);
    }

    fn init_ctx() -> TestCtx {
        TestCtx::default()
    }

    fn finalize_ctx(ctx: TestCtx) {
        std::thread::sleep(Duration::from_secs(1));
        drop(ctx);
    }

    fn create_file_at<S: AsRef<str>>(client: &SmbClient, uri: S, content: S) {
        info!("create_file_at - uri: {}", uri.as_ref());

        let mut reader = Cursor::new(content.as_ref().as_bytes());
        let mut writer = client
            .open_with(
                uri,
                SmbOpenOptions::default()
                    .create(true)
                    .write(true)
                    .mode(0o744),
            )
            .expect("failed to open file");
        assert!(std::io::copy(&mut reader, &mut writer).is_ok());
    }
}
